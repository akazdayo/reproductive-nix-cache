use anyhow::Result;
use std::process::Stdio;
use thiserror::Error;
use tokio::process::{ChildStdout, Command};
use tokio_util::codec::{FramedRead, LinesCodec};

#[derive(Debug, Error)]
pub enum NixBuildError {
    #[error("Failed to get stdout stream.")]
    FailedGetStdoutStream,
}

pub async fn run_build(
    package_ref: &str,
    full_rebuild: bool,
) -> Result<FramedRead<ChildStdout, LinesCodec>, NixBuildError> {
    let mut args: Vec<&str> = vec!["build"];

    if full_rebuild {
        // キャッシュを利用しない
        args.extend(["--rebuild", "--option", "substitute", "false"]);
    }
    args.push(package_ref);
    args.push("--no-link");

    let mut child = Command::new("nix")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start nix process");

    // stdoutへの出力ハンドラを取得
    // let stdout = child.stdout.take().unwrap();
    if let Some(stdout) = child.stdout.take() {
        // こんな感じで使う
        // while let Some(line) = reader.next().await {
        //    println!("{}", line?);
        // }
        Ok(FramedRead::new(stdout, LinesCodec::new()))
    } else {
        Err(NixBuildError::FailedGetStdoutStream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_nix_build_cache() {
        assert!(run_build("nixpkgs#hello", false).await.is_ok())
    }
}
