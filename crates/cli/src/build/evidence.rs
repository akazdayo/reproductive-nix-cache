use crate::build::nix;
use anyhow::{Result, bail};
use chrono::Utc;
use shared::{BuildEvidence, EVIDENCE_SCHEMA_VERSION, Package};

/// Rebuild an output without substitutes, then collect the facts needed to
/// compare it with reports from other builders.
pub async fn generate_evidence(
    package: Package,
    builder_id: String,
    quiet: bool,
) -> Result<BuildEvidence> {
    if builder_id.trim().is_empty() {
        bail!("builder_id must not be empty");
    }

    // Keep the Nix queries sequential: concurrent evaluation can contend for
    // Nix's evaluation cache on a single machine.
    let source = nix::resolve_source(&package.repository).await?;
    let derivation_path = nix::derivation_path(&package).await?;
    nix::build(&package, quiet).await?;
    let output = nix::output_info(&package).await?;

    Ok(BuildEvidence {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        builder_id,
        package,
        source,
        derivation_path,
        output_path: output.output_path,
        nar_hash: output.nar_hash,
        built_at: Utc::now(),
    })
}
