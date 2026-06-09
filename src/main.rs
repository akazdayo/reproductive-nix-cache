use std::collections::HashMap;
use std::process::Command as ProcessCommand;

use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(name = "reproductive-nix-cache")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Build {
        /// Package reference like nixpkgs#hello
        package_ref: String,
        /// Rebuild all dependencies from scratch (no cache, no substitutes)
        #[arg(long)]
        full_rebuild: bool,
    },
}

#[derive(Serialize)]
struct NixEvidence {
    schema: &'static str,
    requested: String,
    resolved: Resolved,
    flake: FlakeEvidence,
    derivation: DerivationEvidence,
    output: OutputEvidence,
}

#[derive(Serialize)]
struct Resolved {
    pname: String,
    version: String,
    system: String,
}

#[derive(Serialize)]
struct FlakeEvidence {
    locked_url: String,
    rev: String,
}

#[derive(Serialize)]
struct DerivationEvidence {
    drv_path: String,
    drv_hash: String,
}

#[derive(Serialize)]
struct OutputEvidence {
    store_path: String,
    nar_hash: String,
}

#[derive(Deserialize)]
struct FlakeMetadata {
    url: String,
    locked: LockedFlake,
}

#[derive(Deserialize)]
struct LockedFlake {
    #[serde(default)]
    rev: Option<String>,
    #[serde(default, rename = "narHash")]
    nar_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct PathInfo {
    path: String,
    nar_hash: String,
}

#[derive(Deserialize)]
struct Derivation {
    system: String,
    env: HashMap<String, String>,
}

struct PackageRef<'a> {
    flake_name: &'a str,
    attr_path: &'a str,
}

fn parse_package_ref(input: &str) -> anyhow::Result<PackageRef<'_>> {
    let (flake_name, attr_path) = input
        .split_once('#')
        .ok_or_else(|| anyhow!("package reference must contain '#'"))?;

    if flake_name.is_empty() {
        return Err(anyhow!("package reference flake name must not be empty"));
    }

    if attr_path.is_empty() {
        return Err(anyhow!("package reference attribute path must not be empty"));
    }

    Ok(PackageRef {
        flake_name,
        attr_path,
    })
}

fn run_nix(args: &[&str]) -> anyhow::Result<String> {
    let output = ProcessCommand::new("nix")
        .args(args)
        .output()
        .map_err(|_| anyhow!("failed to execute nix; is nix installed and on PATH?"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("nix command failed: {stderr}"));
    }

    String::from_utf8(output.stdout).context("nix stdout was not valid UTF-8")
}

fn extract_path_info(
    json: &str,
    label: &str,
) -> anyhow::Result<(String, String)> {
    let map: HashMap<String, serde_json::Value> = serde_json::from_str(json)
        .with_context(|| format!("failed to parse `{label}` output"))?;
    let (path, value) = map
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("`{label}` returned empty"))?;
    let nar_hash = value
        .get("narHash")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok((path, nar_hash))
}

fn run_build(package_ref: &str, full_rebuild: bool) -> anyhow::Result<()> {
    let mut args: Vec<&str> = vec!["build"];
    if full_rebuild {
        args.extend(["--rebuild", "--option", "substitute", "false"]);
    }
    args.push(package_ref);
    args.push("--no-link");

    let status = ProcessCommand::new("nom")
        .args(&args)
        .status()
        .or_else(|_| {
            ProcessCommand::new("nix")
                .args(&args)
                .status()
        })
        .map_err(|_| {
            anyhow!("failed to execute build command; is nix installed and on PATH?")
        })?;

    if !status.success() {
        return Err(anyhow!(
            "build failed with exit code {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}
fn select_derivation(
    mut derivations: HashMap<String, Derivation>,
    drv_path: &str,
) -> anyhow::Result<Derivation> {
    if let Some(derivation) = derivations.remove(drv_path) {
        return Ok(derivation);
    }

    if derivations.len() == 1 {
        return derivations
            .into_values()
            .next()
            .ok_or_else(|| anyhow!("failed to read derivation entry"));
    }

    Err(anyhow!(
        "`nix derivation show` output did not contain derivation `{drv_path}`"
    ))
}

fn required_env_value(env: &HashMap<String, String>, key: &str) -> anyhow::Result<String> {
    env.get(key)
        .cloned()
        .ok_or_else(|| anyhow!("derivation env did not contain `{key}`"))
}

fn build_evidence(package_ref: &str, full_rebuild: bool) -> anyhow::Result<NixEvidence> {
    let parsed = parse_package_ref(package_ref)?;
    let requested = format!("{}#{}", parsed.flake_name, parsed.attr_path);

    let flake_metadata_json = run_nix(&["flake", "metadata", parsed.flake_name, "--json"])?;
    let flake_metadata: FlakeMetadata = serde_json::from_str(&flake_metadata_json)
        .context("failed to parse `nix flake metadata` output")?;

    let drv_path_info_json = run_nix(&["path-info", "--json", "--derivation", package_ref])?;
    let (drv_path, drv_hash) =
        extract_path_info(&drv_path_info_json, "nix path-info --derivation")?;

    let derivation_json = run_nix(&["derivation", "show", &drv_path])?;
    let derivation_map: HashMap<String, Derivation> = serde_json::from_str(&derivation_json)
        .context("failed to parse `nix derivation show` output")?;
    let derivation = select_derivation(derivation_map, &drv_path)?;

    run_build(package_ref, full_rebuild)?;

    let output_path_info_json = run_nix(&["path-info", "--json", package_ref])?;
    let (store_path, nar_hash) =
        extract_path_info(&output_path_info_json, "nix path-info")?;

    Ok(NixEvidence {
        schema: "nix-evidence/v0",
        requested,
        resolved: Resolved {
            pname: required_env_value(&derivation.env, "pname")?,
            version: required_env_value(&derivation.env, "version")?,
            system: derivation.system,
        },
        flake: FlakeEvidence {
            locked_url: flake_metadata.url,
            rev: flake_metadata
                .locked
                .rev
                .or(flake_metadata.locked.nar_hash)
                .unwrap_or_default(),
        },
        derivation: DerivationEvidence {
            drv_path,
            drv_hash,
        },
        output: OutputEvidence {
            store_path,
            nar_hash,
        },
    })
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build { package_ref, full_rebuild } => {
            let evidence = build_evidence(&package_ref, full_rebuild)?;
            println!("{}", serde_json::to_string_pretty(&evidence)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn test_parse_package_ref_valid() {
        let package_ref = parse_package_ref("nixpkgs#hello").expect("valid package ref");
        assert_eq!(package_ref.flake_name, "nixpkgs");
        assert_eq!(package_ref.attr_path, "hello");
    }

    #[test]
    fn test_parse_package_ref_no_hash() {
        assert!(parse_package_ref("nixpkgs").is_err());
    }

    #[test]
    fn test_parse_package_ref_empty_flake() {
        assert!(parse_package_ref("#hello").is_err());
    }

    #[test]
    fn test_parse_package_ref_empty_attr() {
        assert!(parse_package_ref("nixpkgs#").is_err());
    }

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
        assert_eq!(
            metadata.locked.nar_hash.as_deref(),
            Some("sha256-hash")
        );
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
