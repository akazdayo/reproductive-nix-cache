use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Structured evidence for a Nix build — schema, resolved metadata, flake lock, derivation, and output.
#[derive(Serialize)]
pub struct NixEvidence {
    pub schema: &'static str,
    pub requested: String,
    pub resolved: Resolved,
    pub flake: FlakeEvidence,
    pub derivation: DerivationEvidence,
    pub output: OutputEvidence,
}

#[derive(Serialize)]
pub struct Resolved {
    pub pname: String,
    pub version: String,
    pub system: String,
}

#[derive(Serialize)]
pub struct FlakeEvidence {
    pub locked_url: String,
    pub rev: String,
}

#[derive(Serialize)]
pub struct DerivationEvidence {
    pub drv_path: String,
    pub drv_hash: String,
}

#[derive(Serialize)]
pub struct OutputEvidence {
    pub store_path: String,
    pub nar_hash: String,
}

#[derive(Deserialize)]
pub struct FlakeMetadata {
    pub url: String,
    pub locked: LockedFlake,
}

#[derive(Deserialize)]
pub struct LockedFlake {
    #[serde(default)]
    pub rev: Option<String>,
    #[serde(default, rename = "narHash")]
    pub nar_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct PathInfo {
    pub path: String,
    pub nar_hash: String,
}

#[derive(Deserialize)]
pub struct Derivation {
    pub system: String,
    pub env: HashMap<String, String>,
}

pub struct PackageRef<'a> {
    pub flake_name: &'a str,
    pub attr_path: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::HashMap;

    #[test]
    fn test_parse_flake_metadata() {
        let fixture = r#"
        {
            "url": "github:NixOS/nixpkgs/abcdef",
            "locked": {
                "rev": "abcdef123456",
                "narHash": "sha256-hash"
            }
        }
        "#;

        let metadata: FlakeMetadata = serde_json::from_str(fixture).expect("flake metadata JSON");
        assert_eq!(metadata.url, "github:NixOS/nixpkgs/abcdef");
        assert_eq!(metadata.locked.rev.as_deref(), Some("abcdef123456"));
        assert_eq!(metadata.locked.nar_hash.as_deref(), Some("sha256-hash"));
    }

    #[test]
    fn test_parse_path_info_array() {
        let fixture = r#"
        [
            {
                "path": "/nix/store/hash-hello",
                "narHash": "sha256-output"
            }
        ]
        "#;

        let path_infos: Vec<PathInfo> = serde_json::from_str(fixture).expect("path info JSON");
        assert_eq!(path_infos.len(), 1);
        assert_eq!(path_infos[0].path, "/nix/store/hash-hello");
        assert_eq!(path_infos[0].nar_hash, "sha256-output");
    }

    #[test]
    fn test_parse_derivation_show_map() {
        let fixture = r#"
        {
            "/nix/store/hash-hello.drv": {
                "system": "x86_64-linux",
                "env": {
                    "pname": "hello",
                    "version": "2.12.1"
                }
            }
        }
        "#;

        let derivations: HashMap<String, Derivation> =
            serde_json::from_str(fixture).expect("derivation JSON");
        let derivation = derivations
            .get("/nix/store/hash-hello.drv")
            .expect("derivation entry");
        assert_eq!(derivation.system, "x86_64-linux");
        assert_eq!(derivation.env.get("pname").expect("pname"), "hello");
        assert_eq!(derivation.env.get("version").expect("version"), "2.12.1");
    }

    #[test]
    fn test_serialize_nix_evidence() {
        let evidence = NixEvidence {
            schema: "nix-evidence/v0",
            requested: "nixpkgs#hello".to_string(),
            resolved: Resolved {
                pname: "hello".to_string(),
                version: "2.12.1".to_string(),
                system: "x86_64-linux".to_string(),
            },
            flake: FlakeEvidence {
                locked_url: "github:NixOS/nixpkgs/abcdef".to_string(),
                rev: "abcdef123456".to_string(),
            },
            derivation: DerivationEvidence {
                drv_path: "/nix/store/hash-hello.drv".to_string(),
                drv_hash: "sha256-drv".to_string(),
            },
            output: OutputEvidence {
                store_path: "/nix/store/hash-hello".to_string(),
                nar_hash: "sha256-output".to_string(),
            },
        };

        let value: Value = serde_json::to_value(evidence).expect("evidence JSON value");
        assert_eq!(value["schema"], "nix-evidence/v0");
        assert_eq!(value["requested"], "nixpkgs#hello");
        assert_eq!(value["resolved"]["pname"], "hello");
        assert_eq!(value["resolved"]["version"], "2.12.1");
        assert_eq!(value["resolved"]["system"], "x86_64-linux");
        assert_eq!(value["flake"]["locked_url"], "github:NixOS/nixpkgs/abcdef");
        assert_eq!(value["flake"]["rev"], "abcdef123456");
        assert_eq!(value["derivation"]["drv_path"], "/nix/store/hash-hello.drv");
        assert_eq!(value["derivation"]["drv_hash"], "sha256-drv");
        assert_eq!(value["output"]["store_path"], "/nix/store/hash-hello");
        assert_eq!(value["output"]["nar_hash"], "sha256-output");
    }
}
