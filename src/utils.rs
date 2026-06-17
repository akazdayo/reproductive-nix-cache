use anyhow::Result;
use futures_util::StreamExt;
use std::process::Stdio;
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
