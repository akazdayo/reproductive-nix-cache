use crate::build::nix;
use shared::{Evidences, Output, Package};
use crate::utils;
use anyhow::{Result, bail};

pub async fn generate_evidence(
    package: Package,
    evidences: Vec<Evidences>,
    full_rebuild: bool,
    quiet: bool,
) -> Result<Output> {
    let mut nix_child = nix::run_build(&package, full_rebuild).await?;
    let nix_stdio = utils::get_stdio(&mut nix_child)?;
    if let Some(stdout) = nix_stdio.stdout
        && !quiet
    {
        utils::output_readable_stream(utils::pipe_nom(stdout).await?).await?;
    }

    let status = nix_child.wait().await?;
    if !status.success() {
        bail!("nix build failed with exit status: {status}");
    }

    let path_info = nix::get_path_info(&package).await?;

    Ok(Output {
        package,
        evidences: evidences.into_iter().next().unwrap_or(Evidences::Logs),
        nar_hash: path_info.nar_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_nonexistent_package_returns_error() {
        let pkg = Package {
            repository: "nixpkgs".to_string(),
            name: "this-package-should-not-exist-ever-99999".to_string(),
        };
        let result = generate_evidence(pkg, vec![Evidences::Logs], false, true).await;
        assert!(
            result.is_err(),
            "generate_evidence should return Err for nonexistent package"
        );
    }
}
