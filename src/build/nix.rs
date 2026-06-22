use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::process::{Child, Command};

use crate::build::model::Package;

pub async fn run_build(package: &Package, full_rebuild: bool) -> Result<Child> {
    let mut args: Vec<&str> = vec!["build"];

    if full_rebuild {
        // キャッシュを利用しない
        args.extend(["--rebuild", "--option", "substitute", "false"]);
    }
    let package_ref = format!("{}#{}", package.repository, package.name);
    args.push(&package_ref);
    args.push("--no-link");

    let child = Command::new("nix")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start nix process")?;

    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_nix_build_cache() {
        let pkg = Package {
            repository: "nixpkgs".to_string(),
            name: "hello".to_string(),
        };
        assert!(run_build(&pkg, false).await.is_ok())
        // TODO: stderrをハンドルしてないので後で直す
    }
}
