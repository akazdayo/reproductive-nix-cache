use anyhow::{Context, Result, bail};
use serde::{Deserialize, de::DeserializeOwned};
use shared::{Package, ResolvedSource};
use std::{collections::BTreeMap, process::Stdio};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    pub output_path: String,
    pub nar_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NixPathInfo {
    nar_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FlakeMetadata {
    resolved_url: Option<String>,
    url: Option<String>,
    locked: Option<LockedFlake>,
}

#[derive(Debug, Deserialize)]
struct LockedFlake {
    rev: Option<String>,
    #[serde(rename = "narHash")]
    nar_hash: Option<String>,
}

pub async fn resolve_source(repository: &str) -> Result<ResolvedSource> {
    let metadata: FlakeMetadata = run_json(["flake", "metadata", "--json", repository]).await?;
    let locked = metadata.locked.unwrap_or(LockedFlake {
        rev: None,
        nar_hash: None,
    });

    Ok(ResolvedSource {
        resolved_url: metadata
            .resolved_url
            .or(metadata.url)
            .unwrap_or_else(|| repository.to_owned()),
        revision: locked.rev,
        nar_hash: locked.nar_hash,
    })
}

pub async fn derivation_path(package: &Package) -> Result<String> {
    let reference = package.reference();
    let entries: BTreeMap<String, serde_json::Value> = run_json([
        "path-info",
        "--derivation",
        "--json-format",
        "1",
        "--json",
        reference.as_str(),
    ])
    .await?;

    take_only_entry(entries, "nix path-info --derivation")
}

pub async fn build(package: &Package, quiet: bool) -> Result<()> {
    let reference = package.reference();
    let output = if quiet {
        Stdio::null()
    } else {
        Stdio::inherit()
    };
    let error = if quiet {
        Stdio::null()
    } else {
        Stdio::inherit()
    };
    let status = Command::new("nix")
        .args([
            "build",
            "--rebuild",
            "--option",
            "substitute",
            "false",
            reference.as_str(),
            "--no-link",
        ])
        .stdout(output)
        .stderr(error)
        .status()
        .await
        .context("failed to start nix build")?;

    if !status.success() {
        bail!("nix build failed with exit status: {status}");
    }

    Ok(())
}

pub async fn output_info(package: &Package) -> Result<OutputInfo> {
    let reference = package.reference();
    let entries: BTreeMap<String, NixPathInfo> = run_json([
        "path-info",
        "--json-format",
        "1",
        "--json",
        reference.as_str(),
    ])
    .await?;
    let (output_path, info) = take_only_entry_with_value(entries, "nix path-info")?;

    Ok(OutputInfo {
        output_path,
        nar_hash: info.nar_hash,
    })
}

async fn run_json<T, I, S>(args: I) -> Result<T>
where
    T: DeserializeOwned,
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("nix")
        .args(args)
        .output()
        .await
        .context("failed to start nix")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "nix command failed with exit status {}: {stderr}",
            output.status
        );
    }

    serde_json::from_slice(&output.stdout).context("failed to parse JSON from nix")
}

fn take_only_entry<T>(entries: BTreeMap<String, T>, command: &str) -> Result<String> {
    take_only_entry_with_value(entries, command).map(|(key, _)| key)
}

fn take_only_entry_with_value<T>(
    entries: BTreeMap<String, T>,
    command: &str,
) -> Result<(String, T)> {
    if entries.len() != 1 {
        bail!(
            "{command} returned {} entries; expected exactly one",
            entries.len()
        );
    }

    entries
        .into_iter()
        .next()
        .context("nix returned no entries")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_entry_rejects_multiple_nix_results() {
        let entries = BTreeMap::from([("one".into(), ()), ("two".into(), ())]);
        assert!(take_only_entry(entries, "nix path-info").is_err());
    }
}
