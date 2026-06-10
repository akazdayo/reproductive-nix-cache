use std::collections::HashMap;

use anyhow::Context;

use crate::models::{
    Derivation, DerivationEvidence, FlakeEvidence, FlakeMetadata, NixEvidence, OutputEvidence,
    PackageRef, Resolved,
};
use crate::nix;

/// Parse a package reference like `nixpkgs#hello` into its flake name and
/// attribute path components.
pub fn parse_package_ref(input: &str) -> anyhow::Result<PackageRef<'_>> {
    let (flake_name, attr_path) = input
        .split_once('#')
        .ok_or_else(|| anyhow::anyhow!("package reference must contain '#'"))?;

    if flake_name.is_empty() {
        return Err(anyhow::anyhow!(
            "package reference flake name must not be empty"
        ));
    }

    if attr_path.is_empty() {
        return Err(anyhow::anyhow!(
            "package reference attribute path must not be empty"
        ));
    }

    Ok(PackageRef {
        flake_name,
        attr_path,
    })
}

/// Run the full evidence pipeline: resolve, build, and produce structured
/// Nix build evidence.
pub fn build_evidence(package_ref: &str, full_rebuild: bool) -> anyhow::Result<NixEvidence> {
    let parsed = parse_package_ref(package_ref)?;
    let requested = format!("{}#{}", parsed.flake_name, parsed.attr_path);

    let flake_metadata_json =
        nix::run_nix(&["flake", "metadata", parsed.flake_name, "--json"])?;
    let flake_metadata: FlakeMetadata = serde_json::from_str(&flake_metadata_json)
        .context("failed to parse `nix flake metadata` output")?;

    let drv_path_info_json =
        nix::run_nix(&["path-info", "--json", "--derivation", package_ref])?;
    let (drv_path, drv_hash) =
        nix::extract_path_info(&drv_path_info_json, "nix path-info --derivation")?;

    let derivation_json = nix::run_nix(&["derivation", "show", &drv_path])?;
    let derivation_map: HashMap<String, Derivation> = serde_json::from_str(&derivation_json)
        .context("failed to parse `nix derivation show` output")?;
    let derivation = nix::select_derivation(derivation_map, &drv_path)?;

    nix::run_build(package_ref, full_rebuild)?;

    let output_path_info_json = nix::run_nix(&["path-info", "--json", package_ref])?;
    let (store_path, nar_hash) =
        nix::extract_path_info(&output_path_info_json, "nix path-info")?;

    Ok(NixEvidence {
        schema: "nix-evidence/v0",
        requested,
        resolved: Resolved {
            pname: nix::required_env_value(&derivation.env, "pname")?,
            version: nix::required_env_value(&derivation.env, "version")?,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
