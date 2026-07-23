use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const EVIDENCE_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub repository: String,
}

impl Package {
    pub fn reference(&self) -> String {
        format!("{}#{}", self.repository, self.name)
    }
}

/// Immutable information about the flake used when evaluating a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSource {
    pub resolved_url: String,
    pub revision: Option<String>,
    pub nar_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub schema_version: u32,
    pub builder_id: String,
    pub package: Package,
    pub claims: Vec<Claim>,
}

impl Evidence {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != EVIDENCE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported evidence schema version {}; expected {}",
                self.schema_version, EVIDENCE_SCHEMA_VERSION
            ));
        }

        validate_text("builder_id", &self.builder_id, 128)?;
        validate_text("package.repository", &self.package.repository, 2048)?;
        validate_text("package.name", &self.package.name, 1024)?;

        let build_count = self
            .claims
            .iter()
            .filter(|claim| matches!(claim, Claim::Build(_)))
            .count();
        if build_count != 1 {
            return Err(format!(
                "evidence must contain exactly one build claim; found {build_count}"
            ));
        }

        for claim in &self.claims {
            match claim {
                Claim::Build(build) => {
                    validate_text(
                        "build.source.resolved_url",
                        &build.source.resolved_url,
                        4096,
                    )?;
                    validate_optional_text(
                        "build.source.revision",
                        build.source.revision.as_deref(),
                        1024,
                    )?;
                    validate_optional_text(
                        "build.source.nar_hash",
                        build.source.nar_hash.as_deref(),
                        1024,
                    )?;
                    validate_text("build.derivation_path", &build.derivation_path, 4096)?;
                    validate_text(
                        "build.build_statement.output_name",
                        &build.build_statement.output_name,
                        1024,
                    )?;
                    validate_text(
                        "build.build_statement.output_store_path",
                        &build.build_statement.output_store_path,
                        4096,
                    )?;
                    validate_text(
                        "build.build_statement.nar_hash",
                        &build.build_statement.nar_hash,
                        1024,
                    )?;
                    for reference in &build.build_statement.references {
                        validate_text("build.build_statement.references[]", reference, 4096)?;
                    }
                    validate_text(
                        "build.build_statement.closure_root",
                        &build.build_statement.closure_root,
                        4096,
                    )?;
                    validate_optional_text(
                        "build.build_statement.content_addressed",
                        build.build_statement.content_addressed.as_deref(),
                        1024,
                    )?;
                    validate_optional_text(
                        "build.build_statement.build_log_digest",
                        build.build_statement.build_log_digest.as_deref(),
                        1024,
                    )?;
                    validate_optional_text(
                        "build.build_statement.sbom_digest",
                        build.build_statement.sbom_digest.as_deref(),
                        1024,
                    )?;
                    validate_optional_text(
                        "build.build_statement.test_result_digest",
                        build.build_statement.test_result_digest.as_deref(),
                        1024,
                    )?;
                }
                Claim::Log(log) => {
                    if log.finished_at < log.started_at {
                        return Err(
                            "log.finished_at must not be earlier than log.started_at".into()
                        );
                    }
                }
            }
        }

        Ok(())
    }

    pub fn build_claim(&self) -> Option<&BuildClaim> {
        self.claims.iter().find_map(|claim| match claim {
            Claim::Build(build) => Some(build.as_ref()),
            Claim::Log(_) => None,
        })
    }
}

fn validate_text(name: &str, value: &str, max_len: usize) -> Result<(), String> {
    // nameはエラー吐く用かな
    if value.trim().is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if value.len() > max_len {
        return Err(format!("{name} exceeds the maximum length of {max_len}"));
    }
    Ok(())
}

fn validate_optional_text(name: &str, value: Option<&str>, max_len: usize) -> Result<(), String> {
    if let Some(value) = value {
        validate_text(name, value, max_len)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Claim {
    Build(Box<BuildClaim>),
    Log(LogClaim),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildClaim {
    pub source: ResolvedSource,
    pub derivation_path: String,
    pub build_statement: BuildStatement,
    pub built_at: DateTime<Utc>,
}

/// Facts that identify one concrete output produced by a Nix build.
///
/// This is the innermost layer of the attestation model. Execution evidence,
/// attestation results, and collective records can wrap it without flattening
/// output facts into their own envelopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildStatement {
    pub output_name: String,
    pub output_store_path: String,
    pub nar_hash: String,
    pub nar_size: u64,
    pub references: Vec<String>,
    pub closure_root: String,
    /// Nix's content address (`ca` in `nix path-info --json`), when present.
    pub content_addressed: Option<String>,
    /// Digests are optional until the corresponding artifact is collected.
    pub build_log_digest: Option<String>,
    pub sbom_digest: Option<String>,
    pub test_result_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogClaim {
    pub stdout: String,
    pub stderr: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEvidence {
    pub id: i64,
    #[serde(flatten)]
    pub evidence: Evidence,
    pub received_at: DateTime<Utc>,
}

/// The server returns raw evidence records only. Consumers decide how to
/// interpret these facts, including any trust score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceList {
    pub derivation_path: String,
    pub evidences: Vec<StoredEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceReceipt {
    pub inserted: bool,
    pub evidence: StoredEvidence,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn claim_evidence() -> Evidence {
        let started_at = Utc.with_ymd_and_hms(2026, 7, 11, 0, 0, 0).unwrap();
        let finished_at = Utc.with_ymd_and_hms(2026, 7, 11, 0, 1, 0).unwrap();
        Evidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            builder_id: "builder-a".into(),
            package: Package {
                repository: "nixpkgs".into(),
                name: "hello".into(),
            },
            claims: vec![
                Claim::Build(Box::new(BuildClaim {
                    source: ResolvedSource {
                        resolved_url: "flake:nixpkgs".into(),
                        revision: Some("revision".into()),
                        nar_hash: Some("sha256-source".into()),
                    },
                    derivation_path: "/nix/store/hello.drv".into(),
                    build_statement: BuildStatement {
                        output_name: "out".into(),
                        output_store_path: "/nix/store/hello".into(),
                        nar_hash: "sha256-output".into(),
                        nar_size: 1234,
                        references: vec!["/nix/store/glibc".into()],
                        closure_root: "/nix/store/hello".into(),
                        content_addressed: None,
                        build_log_digest: None,
                        sbom_digest: None,
                        test_result_digest: None,
                    },
                    built_at: finished_at,
                })),
                Claim::Log(LogClaim {
                    stdout: "stdout\n".into(),
                    stderr: "stderr\n".into(),
                    started_at,
                    finished_at,
                }),
            ],
        }
    }

    #[test]
    fn package_reference_joins_repository_and_attribute() {
        let package = Package {
            repository: "nixpkgs".into(),
            name: "hello".into(),
        };

        assert_eq!(package.reference(), "nixpkgs#hello");
    }

    #[test]
    fn claims_use_tagged_json_and_round_trip() {
        let evidence = claim_evidence();
        let value = serde_json::to_value(&evidence).unwrap();
        assert_eq!(value["claims"][0]["type"], "build");
        assert_eq!(
            value["claims"][0]["payload"]["build_statement"]["output_store_path"],
            "/nix/store/hello"
        );
        assert_eq!(value["claims"][1]["type"], "log");
        assert_eq!(value["claims"][1]["payload"]["stderr"], "stderr\n");

        let decoded: Evidence = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, evidence);
    }

    #[test]
    fn validation_requires_exactly_one_build_claim() {
        let mut evidence = claim_evidence();
        evidence
            .claims
            .retain(|claim| !matches!(claim, Claim::Build(_)));
        assert_eq!(
            evidence.validate().unwrap_err(),
            "evidence must contain exactly one build claim; found 0"
        );

        let build = claim_evidence().claims.remove(0);
        evidence.claims.push(build.clone());
        evidence.claims.push(build);
        assert_eq!(
            evidence.validate().unwrap_err(),
            "evidence must contain exactly one build claim; found 2"
        );
    }

    #[test]
    fn validation_rejects_empty_fields_and_reversed_log_time() {
        let mut evidence = claim_evidence();
        evidence.builder_id.clear();
        assert_eq!(
            evidence.validate().unwrap_err(),
            "builder_id must not be empty"
        );

        let mut evidence = claim_evidence();
        let Claim::Build(build) = &mut evidence.claims[0] else {
            unreachable!()
        };
        build.build_statement.nar_hash.clear();
        assert_eq!(
            evidence.validate().unwrap_err(),
            "build.build_statement.nar_hash must not be empty"
        );

        let mut evidence = claim_evidence();
        let Claim::Log(log) = &mut evidence.claims[1] else {
            unreachable!()
        };
        std::mem::swap(&mut log.started_at, &mut log.finished_at);
        assert_eq!(
            evidence.validate().unwrap_err(),
            "log.finished_at must not be earlier than log.started_at"
        );
    }

    #[test]
    fn validation_allows_multiple_log_claims() {
        let mut evidence = claim_evidence();
        evidence.claims.push(evidence.claims[1].clone());
        assert!(evidence.validate().is_ok());
    }
}
