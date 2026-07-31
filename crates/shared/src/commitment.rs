use crate::Evidence;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const COMMITMENT_DOMAIN: &[u8] = b"reproductive-nix-cache/evidence/v1\0";
const COMMITMENT_PREFIX: &str = "sha256:";
pub const COMMITMENT_NONCE_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitmentRequest {
    pub builder_id: String,
    pub derivation_path: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitmentReceipt {
    pub inserted: bool,
    pub round: RoundStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceReveal {
    pub round_id: i64,
    pub nonce: String,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundPhase {
    Committing,
    Revealing,
    Completed,
    Expired,
}

impl RoundPhase {
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Completed | Self::Expired)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundStatus {
    pub id: i64,
    pub derivation_path: String,
    pub phase: RoundPhase,
    pub commit_count: usize,
    pub reveal_count: usize,
    pub commit_deadline: DateTime<Utc>,
    pub reveal_deadline: Option<DateTime<Utc>>,
}

pub fn generate_nonce() -> Result<String, String> {
    let mut nonce = [0_u8; COMMITMENT_NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|error| format!("failed to generate nonce: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(nonce))
}

pub fn evidence_commitment(evidence: &Evidence, nonce: &str) -> Result<String, String> {
    let nonce = decode_nonce(nonce)?;
    let canonical = canonical_evidence(evidence)?;
    let mut hash = Sha256::new();
    hash.update(COMMITMENT_DOMAIN);
    hash.update(nonce);
    hash.update(canonical);
    Ok(format!(
        "{COMMITMENT_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(hash.finalize())
    ))
}

pub fn verify_evidence_commitment(
    evidence: &Evidence,
    nonce: &str,
    expected: &str,
) -> Result<(), String> {
    validate_digest(expected)?;
    let actual = evidence_commitment(evidence, nonce)?;
    if actual != expected {
        return Err("evidence does not match the commitment digest".into());
    }
    Ok(())
}

pub fn validate_digest(digest: &str) -> Result<(), String> {
    let encoded = digest
        .strip_prefix(COMMITMENT_PREFIX)
        .ok_or("commitment digest must use the sha256: prefix")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "commitment digest must contain unpadded base64url")?;
    if bytes.len() != 32 {
        return Err("commitment digest must contain a 32-byte SHA-256 value".into());
    }
    Ok(())
}

fn decode_nonce(nonce: &str) -> Result<[u8; COMMITMENT_NONCE_BYTES], String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(nonce)
        .map_err(|_| "nonce must contain unpadded base64url")?;
    bytes
        .try_into()
        .map_err(|_| format!("nonce must contain exactly {COMMITMENT_NONCE_BYTES} bytes"))
}

/// Serializes the fixed Evidence schema as canonical JSON. Evidence only uses
/// integer JSON numbers and ASCII field names, so sorting object keys and using
/// serde_json's shortest scalar encoding implements the RFC 8785 rules needed
/// by this protocol without accepting arbitrary JSON extensions.
fn canonical_evidence(evidence: &Evidence) -> Result<Vec<u8>, String> {
    let value = serde_json::to_value(evidence)
        .map_err(|error| format!("failed to serialize evidence: {error}"))?;
    let mut output = Vec::new();
    write_canonical(&value, &mut output)?;
    Ok(output)
}

fn write_canonical(value: &serde_json::Value, output: &mut Vec<u8>) -> Result<(), String> {
    match value {
        serde_json::Value::Null => output.extend_from_slice(b"null"),
        serde_json::Value::Bool(value) => {
            output.extend_from_slice(if *value { b"true" } else { b"false" })
        }
        serde_json::Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        serde_json::Value::String(value) => output.extend_from_slice(
            serde_json::to_string(value)
                .map_err(|error| format!("failed to serialize evidence string: {error}"))?
                .as_bytes(),
        ),
        serde_json::Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical(value, output)?;
            }
            output.push(b']');
        }
        serde_json::Value::Object(values) => {
            output.push(b'{');
            let mut fields = values.iter().collect::<Vec<_>>();
            fields.sort_by_key(|(name, _)| *name);
            for (index, (name, value)) in fields.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                output.extend_from_slice(
                    serde_json::to_string(name)
                        .map_err(|error| {
                            format!("failed to serialize evidence field name: {error}")
                        })?
                        .as_bytes(),
                );
                output.push(b':');
                write_canonical(value, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BuildClaim, BuildOutput, BuildStatement, Claim, EVIDENCE_SCHEMA_VERSION, Package,
        ResolvedSource,
    };
    use chrono::{TimeZone, Utc};

    fn evidence() -> Evidence {
        Evidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            builder_id: "builder-a".into(),
            package: Package {
                repository: "nixpkgs".into(),
                name: "hello".into(),
            },
            claims: vec![Claim::Build(Box::new(BuildClaim {
                source: ResolvedSource {
                    resolved_url: "flake:nixpkgs".into(),
                    revision: Some("revision".into()),
                    nar_hash: None,
                },
                derivation_path: "/nix/store/hello.drv".into(),
                build_statement: BuildStatement {
                    outputs: vec![BuildOutput {
                        output_name: "out".into(),
                        output_store_path: "/nix/store/hello".into(),
                        nar_hash: "sha256-output".into(),
                        nar_size: 1234,
                        references: vec![],
                        closure_root: "/nix/store/hello".into(),
                        content_addressed: None,
                    }],
                    build_log_digest: None,
                    sbom_digest: None,
                    test_result_digest: None,
                },
                built_at: Utc.with_ymd_and_hms(2026, 7, 31, 0, 0, 0).unwrap(),
            }))],
        }
    }

    #[test]
    fn commitment_is_deterministic_and_verifiable() {
        let nonce = URL_SAFE_NO_PAD.encode([7_u8; COMMITMENT_NONCE_BYTES]);
        let digest = evidence_commitment(&evidence(), &nonce).unwrap();
        assert!(digest.starts_with(COMMITMENT_PREFIX));
        assert!(verify_evidence_commitment(&evidence(), &nonce, &digest).is_ok());
    }

    #[test]
    fn commitment_binds_nonce_and_complete_evidence() {
        let nonce = URL_SAFE_NO_PAD.encode([7_u8; COMMITMENT_NONCE_BYTES]);
        let digest = evidence_commitment(&evidence(), &nonce).unwrap();
        let other_nonce = URL_SAFE_NO_PAD.encode([8_u8; COMMITMENT_NONCE_BYTES]);
        assert!(verify_evidence_commitment(&evidence(), &other_nonce, &digest).is_err());

        let mut changed = evidence();
        changed.package.name = "goodbye".into();
        assert!(verify_evidence_commitment(&changed, &nonce, &digest).is_err());
    }

    #[test]
    fn commitment_rejects_malformed_wire_values() {
        assert!(evidence_commitment(&evidence(), "not base64!").is_err());
        assert!(validate_digest("sha256:short").is_err());
        assert!(validate_digest("sha512:abc").is_err());
    }
}
