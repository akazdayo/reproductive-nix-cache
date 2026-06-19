use crate::{nix, utils};
use anyhow::Result;

#[derive(Debug)]
pub struct Output {
    package: Package,
    evidence: Evidence,
}

#[derive(Debug)]
pub struct Package {
    pub name: String,
    pub repository: String,
}

#[derive(Debug)]
pub enum Evidence {
    Logs,
}

pub async fn generate_evidence(
    package: Package,
    evidences: Vec<Evidence>,
    full_rebuild: bool,
    quiet: bool,
) -> Result<Output> {
    let mut nix_child = nix::run_build(&package, full_rebuild).await?;
    let nix_stdio = utils::get_stdio(&mut nix_child)?;
    if let Some(stdout) = nix_stdio.stdout
        && !quiet
    {
        utils::output_readable_stream(utils::pipe_nom(stdout).await?).await?;
        nix_child.wait().await?;
    } else {
        nix_child.wait().await?;
    }

    Ok(Output {
        package,
        evidence: evidences.into_iter().next().unwrap_or(Evidence::Logs),
    })
}
