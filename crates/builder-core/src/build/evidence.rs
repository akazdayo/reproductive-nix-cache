use crate::build::nix;
use anyhow::{Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use shared::{
    BuildClaim, BuildOutput, BuildStatement, Claim, ClaimKind, EVIDENCE_SCHEMA_VERSION, Evidence,
    LogClaim, Package, ResolvedSource,
};

/// Rebuild an output, optionally using substitutes, then collect the facts
/// needed to compare it with reports from other builders.
pub async fn generate_evidence(
    package: Package,
    builder_id: String,
    quiet: bool,
    substitute: bool,
    enabled_claims: Vec<ClaimKind>,
) -> Result<Evidence> {
    if builder_id.trim().is_empty() {
        bail!("builder_id must not be empty");
    }

    // Keep the Nix queries sequential: concurrent evaluation can contend for
    // Nix's evaluation cache on a single machine.
    let source = nix::resolve_source(&package.repository).await?;
    let derivation_path = nix::derivation_path(&package).await?;
    let run = nix::build(&package, quiet, substitute).await?;
    let outputs = nix::output_info(&run.outputs).await?;

    Ok(compose_evidence(
        package,
        builder_id,
        source,
        derivation_path,
        outputs,
        run,
        ClaimKind::with_required_build(enabled_claims),
    ))
}

fn compose_evidence(
    package: Package,
    builder_id: String,
    source: ResolvedSource,
    derivation_path: String,
    outputs: Vec<nix::OutputInfo>,
    run: nix::BuildRun,
    enabled_claims: Vec<ClaimKind>,
) -> Evidence {
    let log = LogClaim {
        stdout: run.stdout,
        stderr: run.stderr,
        started_at: run.started_at,
        finished_at: run.finished_at,
    };
    let build_log_digest = Some(digest_build_log(&log));
    let outputs = outputs
        .into_iter()
        .map(|output| {
            let closure_root = output.output_path.clone();
            BuildOutput {
                output_name: output.output_name,
                output_store_path: output.output_path,
                nar_hash: output.nar_hash,
                nar_size: output.nar_size,
                references: output.references,
                closure_root,
                content_addressed: output.content_addressed,
            }
        })
        .collect();
    let build = BuildClaim {
        source,
        derivation_path,
        build_statement: BuildStatement {
            outputs,
            build_log_digest,
            sbom_digest: None,
            test_result_digest: None,
        },
        built_at: run.finished_at,
    };
    let mut build = Some(build);
    let mut log = Some(log);
    let claims = enabled_claims
        .into_iter()
        .map(|kind| match kind {
            ClaimKind::Build => Claim::Build(Box::new(
                build.take().expect("build claim was deduplicated"),
            )),
            ClaimKind::Log => Claim::Log(log.take().expect("log claim was deduplicated")),
        })
        .collect();

    Evidence {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        builder_id,
        package,
        claims,
    }
}

fn digest_build_log(log: &LogClaim) -> String {
    #[derive(Serialize)]
    struct BuildLog<'a> {
        stdout: &'a str,
        stderr: &'a str,
    }

    let bytes = serde_json::to_vec(&BuildLog {
        stdout: &log.stdout,
        stderr: &log.stderr,
    })
    .expect("serializing build log strings cannot fail");
    let hash = Sha256::digest(bytes);
    format!("sha256-{}", STANDARD.encode(hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn compose_evidence_always_adds_build_and_deduplicates_claims() {
        let started_at = Utc::now();
        let finished_at = started_at + Duration::seconds(1);
        let evidence = compose_evidence(
            Package {
                repository: "nixpkgs".into(),
                name: "hello".into(),
            },
            "builder-a".into(),
            ResolvedSource {
                resolved_url: "flake:nixpkgs".into(),
                revision: None,
                nar_hash: Some("sha256-source".into()),
            },
            "/nix/store/hello.drv".into(),
            vec![nix::OutputInfo {
                output_name: "out".into(),
                output_path: "/nix/store/hello".into(),
                nar_hash: "sha256-output".into(),
                nar_size: 1234,
                references: vec![],
                content_addressed: None,
            }],
            nix::BuildRun {
                stdout: "stdout\n".into(),
                stderr: "stderr\n".into(),
                started_at,
                finished_at,
                outputs: Default::default(),
            },
            ClaimKind::with_required_build([ClaimKind::Log, ClaimKind::Build, ClaimKind::Log]),
        );

        assert_eq!(evidence.claims.len(), 2);
        assert!(matches!(evidence.claims[0], Claim::Build(_)));
        assert!(matches!(evidence.claims[1], Claim::Log(_)));
    }
}
