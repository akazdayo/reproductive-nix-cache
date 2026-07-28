use crate::{BuildResult, trust::ConsensusStatus};
use std::fmt::Write;

pub fn format_build_summary(result: &BuildResult) -> String {
    let mut summary = String::new();
    let evidence = &result.evidence;
    let trust = &result.trust;
    let registration = if result.receipt.inserted {
        "registered"
    } else {
        "already registered"
    };

    writeln!(summary, "Build complete").unwrap();
    writeln!(summary).unwrap();
    writeln!(summary, "Package       {}", evidence.package.reference()).unwrap();
    writeln!(summary, "Builder       {}", evidence.builder_id).unwrap();
    writeln!(
        summary,
        "Evidence      #{} ({registration})",
        result.receipt.evidence.id
    )
    .unwrap();
    writeln!(summary, "Derivation    {}", trust.derivation_path).unwrap();

    if let Some(build) = evidence.build_claim() {
        writeln!(summary).unwrap();
        writeln!(summary, "Outputs ({})", build.build_statement.outputs.len()).unwrap();
        for output in &build.build_statement.outputs {
            writeln!(summary, "  {}", output.output_name).unwrap();
            writeln!(summary, "    Path       {}", output.output_store_path).unwrap();
            writeln!(summary, "    NAR hash   {}", output.nar_hash).unwrap();
            writeln!(summary, "    NAR size   {}", format_bytes(output.nar_size)).unwrap();
        }
    }

    if let Some(cache) = &result.cache {
        writeln!(summary).unwrap();
        writeln!(summary, "Binary cache").unwrap();
        writeln!(summary, "  Store        {}", cache.store_url).unwrap();
        writeln!(
            summary,
            "  Uploaded     {} outputs",
            cache.output_paths.len()
        )
        .unwrap();
    }

    writeln!(summary).unwrap();
    writeln!(summary, "Trust").unwrap();
    writeln!(summary, "  Score        {}/100", trust.score).unwrap();
    match trust.status {
        ConsensusStatus::Consensus => {
            writeln!(
                summary,
                "  Consensus    {}/{} builders agree",
                trust.matching_builders, trust.total_builders
            )
            .unwrap();
            let current_matches = evidence.build_claim().is_some_and(|build| {
                let mut current = build
                    .build_statement
                    .outputs
                    .iter()
                    .map(|output| crate::trust::OutputHash {
                        output_name: output.output_name.clone(),
                        nar_hash: output.nar_hash.clone(),
                    })
                    .collect::<Vec<_>>();
                current.sort();
                trust.consensus_outputs.as_ref() == Some(&current)
            });
            writeln!(
                summary,
                "  This build   {} consensus",
                if current_matches {
                    "matches"
                } else {
                    "DOES NOT match"
                }
            )
            .unwrap();
        }
        ConsensusStatus::NoUniqueConsensus => {
            writeln!(
                summary,
                "  Consensus    none ({} builders, {} output variants)",
                trust.total_builders,
                trust.variants.len()
            )
            .unwrap();
            writeln!(summary, "  This build   cannot be verified").unwrap();
        }
        ConsensusStatus::NoEvidence => {
            writeln!(summary, "  Consensus    no registry evidence").unwrap();
            writeln!(summary, "  This build   cannot be verified").unwrap();
        }
    }

    summary.pop();
    summary
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CacheUpload;
    use crate::trust::{OutputHash, OutputSetFact, TrustScore};
    use chrono::Utc;
    use shared::{
        BuildClaim, BuildOutput, BuildStatement, Claim, EVIDENCE_SCHEMA_VERSION, Evidence,
        EvidenceList, EvidenceReceipt, Package, ResolvedSource, StoredEvidence,
    };

    fn result() -> BuildResult {
        let evidence = Evidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            builder_id: "builder-a".into(),
            package: Package {
                repository: "nixpkgs".into(),
                name: "hello".into(),
            },
            claims: vec![Claim::Build(Box::new(BuildClaim {
                source: ResolvedSource {
                    resolved_url: "flake:nixpkgs".into(),
                    revision: None,
                    nar_hash: None,
                },
                derivation_path: "/nix/store/hello.drv".into(),
                build_statement: BuildStatement {
                    outputs: vec![BuildOutput {
                        output_name: "out".into(),
                        output_store_path: "/nix/store/hello".into(),
                        nar_hash: "sha256-output".into(),
                        nar_size: 1_234,
                        references: vec![],
                        closure_root: "/nix/store/hello".into(),
                        content_addressed: None,
                    }],
                    build_log_digest: None,
                    sbom_digest: None,
                    test_result_digest: None,
                },
                built_at: Utc::now(),
            }))],
        };
        let stored = StoredEvidence {
            id: 42,
            evidence: evidence.clone(),
            received_at: Utc::now(),
        };
        let consensus_outputs = vec![OutputHash {
            output_name: "out".into(),
            nar_hash: "sha256-output".into(),
        }];

        BuildResult {
            evidence,
            cache: Some(CacheUpload {
                store_url: "s3://nix-cache".into(),
                output_paths: vec!["/nix/store/hello".into()],
            }),
            receipt: EvidenceReceipt {
                inserted: true,
                evidence: stored.clone(),
            },
            facts: EvidenceList {
                derivation_path: "/nix/store/hello.drv".into(),
                evidences: vec![stored],
            },
            trust: TrustScore {
                derivation_path: "/nix/store/hello.drv".into(),
                score: 33,
                consensus_outputs: Some(consensus_outputs.clone()),
                total_builders: 1,
                matching_builders: 1,
                status: ConsensusStatus::Consensus,
                variants: vec![OutputSetFact {
                    outputs: consensus_outputs,
                    builder_count: 1,
                }],
            },
        }
    }

    #[test]
    fn bytes_are_human_readable() {
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_234), "1.2 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn summary_highlights_the_build_and_consensus_result() {
        let summary = format_build_summary(&result());

        assert!(summary.contains("Package       nixpkgs#hello"));
        assert!(summary.contains("Evidence      #42 (registered)"));
        assert!(summary.contains("NAR size   1.2 KiB"));
        assert!(summary.contains("Store        s3://nix-cache"));
        assert!(summary.contains("Uploaded     1 outputs"));
        assert!(summary.contains("Score        33/100"));
        assert!(summary.contains("Consensus    1/1 builders agree"));
        assert!(summary.contains("This build   matches consensus"));
    }

    #[test]
    fn summary_warns_when_the_current_build_differs_from_consensus() {
        let mut result = result();
        result.trust.consensus_outputs.as_mut().unwrap()[0].nar_hash = "sha256-other".into();

        assert!(format_build_summary(&result).contains("This build   DOES NOT match consensus"));
    }
}
