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
    pub repositry: String,
}

#[derive(Debug)]
pub enum Evidence {
    TEE,
    Zk,
    Logs,
}

pub async fn generate_evidence(
    package: Package,
    evidences: Vec<Evidence>,
    full_rebuild: bool,
    quiet: bool,
) -> Result<Output> {
    let nix_stream = nix::run_build(&package.name, full_rebuild).await?;
    let nom_stream = utils::pipe_nom(nix_stream).await?;
    if !quiet {
        utils::output_readable_stream(nom_stream).await?;
    }

    Ok(Output {
        package,
        evidence: evidences.into_iter().next().unwrap_or(Evidence::Logs),
    })
}
