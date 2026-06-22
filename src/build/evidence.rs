use crate::build::model::{Evidences, Output, Package};
use crate::build::nix;
use crate::utils;
use anyhow::Result;

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
        nix_child.wait().await?;
    } else {
        nix_child.wait().await?;
    }

    Ok(Output {
        package,
        evidences: evidences.into_iter().next().unwrap_or(Evidences::Logs),
    })
}
