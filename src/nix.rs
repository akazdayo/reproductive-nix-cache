use std::collections::HashMap;
use std::process::Command as ProcessCommand;

use anyhow::{Context, anyhow};

use crate::models::Derivation;

/// Run `nix` with the given arguments, returning stdout as a UTF-8 string.
pub fn run_nix(args: &[&str]) -> anyhow::Result<String> {
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

/// Build the given package reference.  Falls back from `nom` to `nix` if
/// `nom` is not on PATH.
pub fn run_build(package_ref: &str, full_rebuild: bool) -> anyhow::Result<()> {
    let mut args: Vec<&str> = vec!["build"];
    if full_rebuild {
        args.extend(["--rebuild", "--option", "substitute", "false"]);
    }
    args.push(package_ref);
    args.push("--no-link");

    let status = ProcessCommand::new("nom")
        .args(&args)
        .status()
        .or_else(|_| ProcessCommand::new("nix").args(&args).status())
        .map_err(|_| anyhow!("failed to execute build command; is nix installed and on PATH?"))?;

    if !status.success() {
        return Err(anyhow!(
            "build failed with exit code {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// From `nix path-info --json` / `nix path-info --json --derivation` output.
///
/// Expects a JSON object like `{ "/nix/store/...": { "narHash": "..." } }`
/// and returns the store path and narHash.
pub fn extract_path_info(json: &str, label: &str) -> anyhow::Result<(String, String)> {
    let map: HashMap<String, serde_json::Value> =
        serde_json::from_str(json).with_context(|| format!("failed to parse `{label}` output"))?;
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

/// Select the derivation for `drv_path` from the `nix derivation show` map.
///
/// If the exact path is present it is used; otherwise, if there is exactly
/// one entry in the map, that entry is returned as a fallback.
pub fn select_derivation(
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

/// Extract a required key from the derivation environment.
pub fn required_env_value(env: &HashMap<String, String>, key: &str) -> anyhow::Result<String> {
    env.get(key)
        .cloned()
        .ok_or_else(|| anyhow!("derivation env did not contain `{key}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nix_help() -> anyhow::Result<()> {
        let resp = run_nix(&["--help"]);
        if let Err(err) = resp {
            panic!("ERROR: {:?}", err)
        }
        println!("{:?}", resp?);
        Ok(())
    }

    #[test]
    fn test_nix_run() {
        let resp = run_nix(&["run", "nixpkgs#hello", "--", "-t"]);
        match resp {
            Ok(value) => {
                assert_eq!(value, "hello, world\n");
            }
            Err(err) => {
                panic!("ERROR: {:?}", err)
            }
        }
    }

    #[test]
    fn test_nix_build_cache() {}
}
