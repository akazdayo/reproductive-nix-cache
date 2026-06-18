use crate::{nix, utils};
use anyhow::Result;

#[derive(Debug)]
pub struct Output {
    package: Package,
    evidence: Evidence,
}

#[derive(Debug)]
pub struct Package {
    name: String,
    repositry: String,
}

#[derive(Debug)]
pub enum Evidence {
    TEE,
    Zk,
    Logs,
}

pub async fn generate_evidence(package: Package, evidences: Vec<Evidence>) -> Result<Output> {
    let nix_stream = nix::run_build(&package.name, true).await?;
    let nom_stream = utils::pipe_nom(nix_stream).await?;
    utils::output_readable_stream(nom_stream).await?;

    Ok(Output {
        package,
        evidence: evidences.into_iter().next().unwrap_or(Evidence::Logs),
    })
}
