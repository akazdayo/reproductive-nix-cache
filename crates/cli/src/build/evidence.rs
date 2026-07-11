use crate::build::nix;
use crate::claims::ClaimKind;
use anyhow::{Result, bail};
use shared::{
    BuildClaim, Claim, EVIDENCE_SCHEMA_VERSION, Evidence, LogClaim, Package, ResolvedSource,
};

/// Rebuild an output without substitutes, then collect the facts needed to
/// compare it with reports from other builders.
pub async fn generate_evidence(
    package: Package,
    builder_id: String,
    quiet: bool,
    enabled_claims: Vec<ClaimKind>,
) -> Result<Evidence> {
    if builder_id.trim().is_empty() {
        bail!("builder_id must not be empty");
    }

    // Keep the Nix queries sequential: concurrent evaluation can contend for
    // Nix's evaluation cache on a single machine.
    let source = nix::resolve_source(&package.repository).await?;
    let derivation_path = nix::derivation_path(&package).await?;
    let run = nix::build(&package, quiet).await?;
    let output = nix::output_info(&package).await?;

    Ok(compose_evidence(
        package,
        builder_id,
        source,
        derivation_path,
        output,
        run,
        ClaimKind::with_required_build(enabled_claims),
    ))
}

fn compose_evidence(
    package: Package,
    builder_id: String,
    source: ResolvedSource,
    derivation_path: String,
    output: nix::OutputInfo,
    run: nix::BuildRun,
    enabled_claims: Vec<ClaimKind>,
) -> Evidence {
    let build = BuildClaim {
        source,
        derivation_path,
        output_path: output.output_path,
        nar_hash: output.nar_hash,
        built_at: run.finished_at,
    };
    let log = LogClaim {
        stdout: run.stdout,
        stderr: run.stderr,
        started_at: run.started_at,
        finished_at: run.finished_at,
    };
    let mut build = Some(build);
    let mut log = Some(log);
    let claims = enabled_claims
        .into_iter()
        .map(|kind| match kind {
            ClaimKind::Build => Claim::Build(build.take().expect("build claim was deduplicated")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::ClaimKind;
    use chrono::{Duration, Utc};

    #[test]
    fn compose_evidence_always_creates_build_claim_without_optional_log() {
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
            nix::OutputInfo {
                output_path: "/nix/store/hello".into(),
                nar_hash: "sha256-output".into(),
            },
            nix::BuildRun {
                stdout: "stdout\n".into(),
                stderr: "stderr\n".into(),
                started_at,
                finished_at,
            },
            vec![ClaimKind::Build],
        );

        assert_eq!(evidence.claims.len(), 1);
        assert!(matches!(evidence.claims[0], Claim::Build(_)));
    }

    #[test]
    fn compose_evidence_adds_log_when_selected() {
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
            nix::OutputInfo {
                output_path: "/nix/store/hello".into(),
                nar_hash: "sha256-output".into(),
            },
            nix::BuildRun {
                stdout: "stdout\n".into(),
                stderr: "stderr\n".into(),
                started_at,
                finished_at,
            },
            vec![ClaimKind::Build, ClaimKind::Log],
        );

        assert!(matches!(evidence.claims[0], Claim::Build(_)));
        assert!(matches!(evidence.claims[1], Claim::Log(_)));
        let Claim::Log(log) = &evidence.claims[1] else {
            unreachable!()
        };
        assert_eq!(log.stdout, "stdout\n");
        assert_eq!(log.stderr, "stderr\n");
        assert_eq!(log.started_at, started_at);
        assert_eq!(log.finished_at, finished_at);
    }
}
