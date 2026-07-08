use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::process::Stdio;
use tokio::process::{Child, Command};

use crate::build::model::NixPathInfo;
use shared::Package;

async fn run_shell(command: &str, args: Vec<&str>) -> Result<Child> {
    let child = Command::new(command)
        .args(args)
        .stdout(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to start shell command process")?;

    Ok(child)
}

pub async fn run_build(package: &Package, full_rebuild: bool) -> Result<Child> {
    let mut args: Vec<&str> = vec!["build"];

    if full_rebuild {
        // キャッシュを利用しない
        args.extend(["--rebuild", "--option", "substitute", "false"]);
    }
    let package_ref = format!("{}#{}", package.repository, package.name);
    args.push(&package_ref);
    args.push("--no-link");

    let child = run_shell("nix", args).await?;

    Ok(child)
}

pub async fn get_path_info(package: &Package) -> Result<NixPathInfo> {
    let package_ref = format!("{}#{}", package.repository, package.name);

    let child = run_shell("nix", ["path-info", "--json", &package_ref].to_vec()).await?;
    let output = child.wait_with_output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("nix path-info failed: {stderr}");
    }

    let entries: HashMap<String, NixPathInfo> =
        serde_json::from_slice(&output.stdout).context("failed to parse nix path-info JSON")?;

    entries
        .into_values()
        .next()
        .ok_or_else(|| anyhow::anyhow!("nix path-info returned empty output"))
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
        let mut child = run_build(&pkg, false).await.unwrap();
        let status = child.wait().await.unwrap();
        assert!(status.success(), "nix build should succeed");
    }

    #[tokio::test]
    async fn test_nix_build_nonexistent_package_fails() {
        let pkg = Package {
            repository: "nixpkgs".to_string(),
            name: "this-package-should-not-exist-ever-99999".to_string(),
        };
        let mut child = run_build(&pkg, false).await.unwrap();
        let status = child.wait().await.unwrap();
        assert!(
            !status.success(),
            "build of nonexistent package should fail"
        );
    }
}
