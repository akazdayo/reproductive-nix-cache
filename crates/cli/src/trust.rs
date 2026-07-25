use serde::Serialize;
use shared::EvidenceList;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustScore {
    pub derivation_path: String,
    pub score: u8,
    pub consensus_outputs: Option<Vec<OutputHash>>,
    pub total_builders: usize,
    pub matching_builders: usize,
    pub status: ConsensusStatus,
    pub variants: Vec<OutputSetFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsensusStatus {
    NoEvidence,
    NoUniqueConsensus,
    Consensus,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct OutputHash {
    pub output_name: String,
    pub nar_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutputSetFact {
    pub outputs: Vec<OutputHash>,
    pub builder_count: usize,
}

/// Calculate a local, explainable score from raw registry facts.
///
/// A unique leading output set gets 33, 67, or 100 points for one, two, or at
/// least three matching builders; disagreement multiplies that value by the
/// leading set's share of all unique builders. A tie deliberately has no
/// consensus.
pub fn calculate_trust(facts: &EvidenceList) -> TrustScore {
    let mut variants: BTreeMap<Vec<OutputHash>, BTreeSet<String>> = BTreeMap::new();
    let mut all_builders = BTreeSet::new();
    for stored in &facts.evidences {
        let Some(build) = stored.evidence.build_claim() else {
            continue;
        };
        all_builders.insert(stored.evidence.builder_id.clone());
        let mut outputs = build
            .build_statement
            .outputs
            .iter()
            .map(|output| OutputHash {
                output_name: output.output_name.clone(),
                nar_hash: output.nar_hash.clone(),
            })
            .collect::<Vec<_>>();
        outputs.sort();
        variants
            .entry(outputs)
            .or_default()
            .insert(stored.evidence.builder_id.clone());
    }

    let variants = variants
        .into_iter()
        .map(|(outputs, builders)| OutputSetFact {
            outputs,
            builder_count: builders.len(),
        })
        .collect::<Vec<_>>();
    let total_builders = all_builders.len();
    let Some(maximum) = variants.iter().map(|variant| variant.builder_count).max() else {
        return TrustScore {
            derivation_path: facts.derivation_path.clone(),
            score: 0,
            consensus_outputs: None,
            total_builders: 0,
            matching_builders: 0,
            status: ConsensusStatus::NoEvidence,
            variants,
        };
    };
    let leaders = variants
        .iter()
        .filter(|variant| variant.builder_count == maximum)
        .collect::<Vec<_>>();
    if leaders.len() != 1 {
        return TrustScore {
            derivation_path: facts.derivation_path.clone(),
            score: 0,
            consensus_outputs: None,
            total_builders,
            matching_builders: 0,
            status: ConsensusStatus::NoUniqueConsensus,
            variants,
        };
    }

    let matching_builders = maximum;
    let score = rounded_score(matching_builders, total_builders);
    TrustScore {
        derivation_path: facts.derivation_path.clone(),
        score,
        consensus_outputs: Some(leaders[0].outputs.clone()),
        total_builders,
        matching_builders,
        status: ConsensusStatus::Consensus,
        variants,
    }
}

fn rounded_score(matching_builders: usize, total_builders: usize) -> u8 {
    debug_assert!(matching_builders > 0);
    debug_assert!(total_builders > 0);
    let matching = matching_builders as u128;
    let total = total_builders as u128;
    let numerator = 100 * matching * matching.min(3);
    let denominator = 3 * total;
    ((numerator + denominator / 2) / denominator) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use shared::{
        BuildClaim, BuildOutput, BuildStatement, Claim, EVIDENCE_SCHEMA_VERSION, Evidence, Package,
        ResolvedSource, StoredEvidence,
    };

    fn facts(reports: &[(&str, &str)]) -> EvidenceList {
        EvidenceList {
            derivation_path: "/nix/store/example.drv".into(),
            evidences: reports
                .iter()
                .enumerate()
                .map(|(index, (builder_id, nar_hash))| StoredEvidence {
                    id: index as i64,
                    evidence: Evidence {
                        schema_version: EVIDENCE_SCHEMA_VERSION,
                        builder_id: (*builder_id).into(),
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
                            derivation_path: "/nix/store/example.drv".into(),
                            build_statement: BuildStatement {
                                outputs: vec![BuildOutput {
                                    output_name: "out".into(),
                                    output_store_path: "/nix/store/example".into(),
                                    nar_hash: (*nar_hash).into(),
                                    nar_size: 1234,
                                    references: vec![],
                                    closure_root: "/nix/store/example".into(),
                                    content_addressed: None,
                                }],
                                build_log_digest: None,
                                sbom_digest: None,
                                test_result_digest: None,
                            },
                            built_at: Utc::now(),
                        }))],
                    },
                    received_at: Utc::now(),
                })
                .collect(),
        }
    }

    #[test]
    fn scores_full_consensus_by_distinct_builder_count() {
        assert_eq!(calculate_trust(&facts(&[("a", "same")])).score, 33);
        assert_eq!(
            calculate_trust(&facts(&[("a", "same"), ("b", "same")])).score,
            67
        );
        assert_eq!(
            calculate_trust(&facts(&[("a", "same"), ("b", "same"), ("c", "same")])).score,
            100
        );
    }

    #[test]
    fn disagreement_penalises_or_removes_consensus() {
        assert_eq!(
            calculate_trust(&facts(&[
                ("a", "same"),
                ("b", "same"),
                ("c", "same"),
                ("d", "other"),
            ]))
            .score,
            75
        );
        let tied = calculate_trust(&facts(&[("a", "one"), ("b", "two")]));
        assert_eq!(tied.score, 0);
        assert_eq!(tied.status, ConsensusStatus::NoUniqueConsensus);
    }

    #[test]
    fn consensus_compares_the_complete_output_set() {
        let mut facts = facts(&[("a", "same"), ("b", "same")]);
        for (stored, man_hash) in facts
            .evidences
            .iter_mut()
            .zip(["sha256-man-one", "sha256-man-two"])
        {
            stored
                .evidence
                .claims
                .iter_mut()
                .find_map(|claim| match claim {
                    Claim::Build(build) => Some(build),
                    Claim::Log(_) => None,
                })
                .unwrap()
                .build_statement
                .outputs
                .push(BuildOutput {
                    output_name: "man".into(),
                    output_store_path: "/nix/store/example-man".into(),
                    nar_hash: man_hash.into(),
                    nar_size: 567,
                    references: vec![],
                    closure_root: "/nix/store/example-man".into(),
                    content_addressed: None,
                });
        }

        let score = calculate_trust(&facts);
        assert_eq!(score.score, 0);
        assert_eq!(score.status, ConsensusStatus::NoUniqueConsensus);
    }

    #[test]
    fn repeated_reports_from_one_builder_do_not_increase_score() {
        let score = calculate_trust(&facts(&[("a", "same"), ("a", "same")]));
        assert_eq!(score.total_builders, 1);
        assert_eq!(score.matching_builders, 1);
        assert_eq!(score.score, 33);
    }
}
