use crate::nix;
use anyhow::Result;

#[derive(Debug)]
struct Output {
    package: Package,
    evidence: Evidence,
}

#[derive(Debug)]
struct Package {
    name: String,
    repositry: String,
}

#[derive(Debug)]
enum Evidence {
    TEE,
    Zk,
    Logs,
}

pub async fn generate_evidence(package: Package, evidences: Vec<Evidence>) -> Result<Output> {
    let build = nix::run_build(&package.name, true).await;
    //nix::wait_build(build).await?;
    Ok(Output {
        package,
        evidence: evidences.into_iter().next().unwrap_or(Evidence::Logs),
    })
}
