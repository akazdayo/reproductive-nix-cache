use anyhow::{Context, anyhow};
use std::process::{Command as ProcessCommand, Stdio};
use tokio::process::{ChildStdout, Command};
use tokio_util::codec::{FramedRead, LinesCodec};

/// Run `nix` with the given arguments, returning stdout as a UTF-8 string.
pub fn run_nix(args: &[&str]) -> anyhow::Result<String> {
    let output = ProcessCommand::new("nix")
        .args(args)
        .output()
        .map_err(|_| anyhow!("failed to execute nix; is nix installed and on PATH?"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("nix command failed: {stderr}"));
    }

    String::from_utf8(output.stdout).context("nix stdout was not valid UTF-8")
}

#[derive(Debug)]
pub enum NixBuildError {
    FailedGetStdoutStream,
}

pub async fn run_build(
    package_ref: &str,
    full_rebuild: bool,
) -> anyhow::Result<FramedRead<ChildStdout, LinesCodec>, NixBuildError> {
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

    #[test]
    fn test_nix_help() {
        assert!(run_nix(&["--help"]).is_ok())
    }

    #[test]
    fn test_nix_run() {
        let resp = run_nix(&["run", "nixpkgs#hello", "--", "-t"]);
        assert_eq!(resp.unwrap(), "hello, world\n");
    }

    #[tokio::test]
    async fn test_nix_build_cache() {
        assert!(run_build("nixpkgs#hello", false).await.is_ok())
    }
}
