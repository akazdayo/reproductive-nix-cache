use anyhow::Result;
use futures_util::StreamExt;
use tokio::process::ChildStdout;
use tokio_util::codec::{FramedRead, LinesCodec};

pub async fn output_readable_stream(mut stream: FramedRead<ChildStdout, LinesCodec>) -> Result<()> {
    while let Some(line) = stream.next().await {
        println!("{}", line?);
    }
    Ok(())
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
