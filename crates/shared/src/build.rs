use crate::Package;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    Build,
    Log,
}

impl ClaimKind {
    pub fn with_required_build(requested: impl IntoIterator<Item = Self>) -> Vec<Self> {
        let mut enabled = vec![Self::Build];
        for kind in requested {
            if !enabled.contains(&kind) {
                enabled.push(kind);
            }
        }
        enabled
    }
}

impl fmt::Display for ClaimKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Build => "build",
            Self::Log => "log",
        })
    }
}

impl FromStr for ClaimKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "build" => Ok(Self::Build),
            "log" => Ok(Self::Log),
            _ => Err("claim must be one of: build, log".into()),
        }
    }
}

/// A build instruction accepted by the round manager and forwarded unchanged
/// to every configured builder node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildCommand {
    pub package_ref: String,
    #[serde(default)]
    pub substitute: bool,
    #[serde(default)]
    pub claims: Vec<ClaimKind>,
}

impl BuildCommand {
    pub fn validate(&self) -> Result<(), String> {
        Package::parse_reference(&self.package_ref).map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildNodeReceipt {
    pub builder_id: String,
    pub round_id: i64,
    pub evidence_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildQueueReceipt {
    pub job_id: u64,
    pub queued: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildDispatchOutcome {
    pub node: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builder_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BuildDispatchOutcome {
    pub fn succeeded(node: String, receipt: BuildNodeReceipt) -> Self {
        Self {
            node,
            success: true,
            builder_id: Some(receipt.builder_id),
            round_id: Some(receipt.round_id),
            evidence_id: Some(receipt.evidence_id),
            error: None,
        }
    }

    pub fn failed(node: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            success: false,
            builder_id: None,
            round_id: None,
            evidence_id: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildDispatchResponse {
    pub builders: Vec<BuildDispatchOutcome>,
}

impl BuildDispatchResponse {
    pub fn has_success(&self) -> bool {
        self.builders.iter().any(|builder| builder.success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_defaults_to_reproducible_build_claim_only() {
        let command: BuildCommand =
            serde_json::from_str(r#"{"package_ref":"nixpkgs#hello"}"#).unwrap();

        assert!(!command.substitute);
        assert!(command.claims.is_empty());
        assert!(command.validate().is_ok());
        assert_eq!(
            ClaimKind::with_required_build(command.claims),
            vec![ClaimKind::Build]
        );
    }

    #[test]
    fn command_rejects_invalid_package_reference_and_unknown_fields() {
        let command = BuildCommand {
            package_ref: "nixpkgs".into(),
            substitute: false,
            claims: vec![],
        };
        assert!(command.validate().is_err());
        assert!(
            serde_json::from_str::<BuildCommand>(
                r#"{"package_ref":"nixpkgs#hello","builder_id":"spoofed"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn claim_list_always_contains_one_build_claim() {
        assert_eq!(
            ClaimKind::with_required_build([ClaimKind::Log, ClaimKind::Build, ClaimKind::Log,]),
            vec![ClaimKind::Build, ClaimKind::Log]
        );
    }
}
