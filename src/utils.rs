use crate::build_test::Package;
use anyhow::Result;
use futures_util::StreamExt;
use regex::Regex;
use std::process::Stdio;
use std::sync::OnceLock;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::{ChildStdout, Command};
use tokio_util::codec::{FramedRead, LinesCodec};

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

#[derive(Debug, Error)]
enum ParseNixRepositoryError {
    #[error("Failed to parse nix repository")]
    FailedToParse,
}

pub fn parse_nix_repository(input: &str) -> Result<Option<Package>, ParseNixRepositoryError> {
    static RE: OnceLock<Regex> = OnceLock::new();

    let re = RE.get_or_init(|| Regex::new(r"^(?P<repo>[^#\s]+)#(?P<package>[^#\s]+)$").unwrap());

    if let Some(caps) = re.captures(input) {
        if let (Some(repo), Some(package)) = (caps.name("repo"), caps.name("package")) {
            return Ok(Some(Package {
                repository: repo.as_str().to_string(),
                name: package.as_str().to_string(),
            }));
        } else {
            return Ok(None);
        }
    }
    Err(ParseNixRepositoryError::FailedToParse)
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
