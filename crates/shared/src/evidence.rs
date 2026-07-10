use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const EVIDENCE_SCHEMA_VERSION: u32 = 1;

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

/// A claim made by a builder after it has rebuilt one Nix output locally.
///
/// This format deliberately contains no TEE attestation or signature. It is a
/// transport format for the prototype, not a cryptographic proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildEvidence {
    pub schema_version: u32,
    pub builder_id: String,
    pub package: Package,
    pub source: ResolvedSource,
    pub derivation_path: String,
    pub output_path: String,
    pub nar_hash: String,
    pub built_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEvidence {
    pub id: i64,
    #[serde(flatten)]
    pub evidence: BuildEvidence,
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

    #[test]
    fn package_reference_joins_repository_and_attribute() {
        let package = Package {
            repository: "nixpkgs".into(),
            name: "hello".into(),
        };

        assert_eq!(package.reference(), "nixpkgs#hello");
    }
}
