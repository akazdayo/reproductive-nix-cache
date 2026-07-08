use anyhow::Result;
use shared::Package;
use futures_util::StreamExt;
use regex::Regex;
use std::process::Stdio;
use std::sync::OnceLock;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

pub async fn output_readable_stream(mut stream: FramedRead<ChildStdout, LinesCodec>) -> Result<()> {
    while let Some(line) = stream.next().await {
        println!("{}", line?);
    }
    Ok(())
}

pub async fn pipe_nom(
    mut stream: FramedRead<ChildStdout, LinesCodec>,
) -> Result<FramedRead<ChildStdout, LinesCodec>> {
    let mut child = Command::new("nom")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    let mut stdin = child.stdin.take().expect("nom stdin should be piped");
    tokio::spawn(async move {
        while let Some(line) = stream.next().await {
            let line = line?;
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
        }
        stdin.shutdown().await?;
        anyhow::Ok(())
    });

    let stdout = child.stdout.take().expect("nom stdout should be piped");
    Ok(FramedRead::new(stdout, LinesCodec::new()))
}

pub fn parse_nix_repository(input: &str) -> Option<Package> {
    static RE: OnceLock<Regex> = OnceLock::new();

    let re = RE.get_or_init(|| Regex::new(r"^(?P<repo>[^#\s]+)#(?P<package>[^#\s]+)$").unwrap());

    let caps = re.captures(input)?;

    Some(Package {
        repository: caps.name("repo")?.as_str().to_string(),
        name: caps.name("package")?.as_str().to_string(),
    })
}

pub struct ShellOutput {
    pub stdout: Option<FramedRead<ChildStdout, LinesCodec>>,
    pub stderr: Option<FramedRead<ChildStderr, LinesCodec>>,
    pub stdin: Option<FramedWrite<ChildStdin, LinesCodec>>,
}

pub fn get_stdio(child: &mut Child) -> Result<ShellOutput> {
    let stdout = match child.stdout.take() {
        Some(val) => Some(val),
        None => None,
    };

    let stderr = match child.stderr.take() {
        Some(val) => Some(val),
        None => None,
    };

    let stdin = match child.stdin.take() {
        Some(val) => Some(val),
        None => None,
    };

    Ok(ShellOutput {
        stdout: match stdout {
            Some(val) => Some(FramedRead::new(val, LinesCodec::new())),
            None => None,
        },
        stderr: match stderr {
            Some(val) => Some(FramedRead::new(val, LinesCodec::new())),
            None => None,
        },
        stdin: match stdin {
            Some(val) => Some(FramedWrite::new(val, LinesCodec::new())),
            None => None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::process::Command;

    #[tokio::test]
    async fn test_stdout_stream() -> Result<()> {
        let mut child = Command::new("printf")
            .arg("first line\nsecond line\n")
            .stdout(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout should be piped");
        let stream = FramedRead::new(stdout, LinesCodec::new());

        output_readable_stream(stream).await?;
        assert!(child.wait().await?.success());

        Ok(())
    }
}
