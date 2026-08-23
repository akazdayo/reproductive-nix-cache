use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, de::DeserializeOwned};
use shared::{Package, ResolvedSource};
use std::{collections::BTreeMap, ffi::OsStr, process::Stdio};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    pub output_name: String,
    pub output_path: String,
    pub nar_hash: String,
    pub nar_size: u64,
    pub references: Vec<String>,
    pub content_addressed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRun {
    pub stdout: String,
    pub stderr: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub outputs: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct NixBuildResult {
    outputs: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NixPathInfo {
    nar_hash: String,
    nar_size: u64,
    references: Vec<String>,
    ca: Option<String>,
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

pub async fn build(package: &Package, quiet: bool, substitute: bool) -> Result<BuildRun> {
    let reference = package.reference();
    // Rebuild requires an existing result. Bootstrap a fresh node with an
    // ordinary build, then perform the independent rebuild used as evidence.
    run_build(initial_build_args(&reference, substitute), quiet).await?;
    run_build(rebuild_args(&reference, substitute), quiet).await
}

async fn run_build<I, S>(args: I, quiet: bool) -> Result<BuildRun>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let started_at = Utc::now();
    let mut child = Command::new("nix")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start nix build")?;
    let stdout = child.stdout.take().context("nix stdout was not piped")?;
    let stderr = child.stderr.take().context("nix stderr was not piped")?;
    let wait = async move { child.wait().await.context("failed to wait for nix build") };
    let (stdout, stderr, status) = tokio::try_join!(
        capture_stream(stdout, tokio::io::sink(), false),
        capture_stream(stderr, tokio::io::stderr(), !quiet),
        wait,
    )?;
    let finished_at = Utc::now();

    if !status.success() {
        bail!(
            "nix build failed with exit status {status}: {}",
            stderr.trim()
        );
    }
    let outputs = parse_build_outputs(&stdout)?;

    Ok(BuildRun {
        stdout,
        stderr,
        started_at,
        finished_at,
        outputs,
    })
}

fn initial_build_args(reference: &str, substitute: bool) -> [&str; 7] {
    [
        "build",
        "--option",
        "substitute",
        if substitute { "true" } else { "false" },
        reference,
        "--no-link",
        "--json",
    ]
}

fn rebuild_args(reference: &str, substitute: bool) -> [&str; 8] {
    [
        "build",
        "--rebuild",
        "--option",
        "substitute",
        if substitute { "true" } else { "false" },
        reference,
        "--no-link",
        "--json",
    ]
}

fn parse_build_outputs(stdout: &str) -> Result<BTreeMap<String, String>> {
    let mut results: Vec<NixBuildResult> =
        serde_json::from_str(stdout).context("failed to parse JSON from nix build")?;
    if results.len() != 1 {
        bail!(
            "nix build returned {} build results; expected exactly one",
            results.len()
        );
    }
    let outputs = results.pop().expect("length was checked").outputs;
    if outputs.is_empty() {
        bail!("nix build returned no outputs");
    }
    Ok(outputs)
}

async fn capture_stream<R, W>(mut reader: R, mut writer: W, echo: bool) -> Result<String>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut collected = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .context("failed to read build output")?;
        if read == 0 {
            break;
        }
        collected.extend_from_slice(&buffer[..read]);
        if echo {
            writer
                .write_all(&buffer[..read])
                .await
                .context("failed to display build output")?;
            writer
                .flush()
                .await
                .context("failed to flush build output")?;
        }
    }

    Ok(String::from_utf8_lossy(&collected).into_owned())
}

pub async fn output_info(outputs: &BTreeMap<String, String>) -> Result<Vec<OutputInfo>> {
    if outputs.is_empty() {
        bail!("cannot query metadata for an empty output set");
    }
    let mut args = vec![
        "path-info".to_owned(),
        "--json-format".to_owned(),
        "1".to_owned(),
        "--json".to_owned(),
    ];
    args.extend(outputs.values().cloned());
    let entries: BTreeMap<String, NixPathInfo> = run_json(args).await?;

    collect_output_info(outputs, entries)
}

fn collect_output_info(
    outputs: &BTreeMap<String, String>,
    mut entries: BTreeMap<String, NixPathInfo>,
) -> Result<Vec<OutputInfo>> {
    let mut result = Vec::with_capacity(outputs.len());
    for (output_name, output_path) in outputs {
        let info = entries.remove(output_path).with_context(|| {
            format!("nix path-info did not return the built output {output_name} at {output_path}")
        })?;
        result.push(OutputInfo {
            output_name: output_name.clone(),
            output_path: output_path.clone(),
            nar_hash: info.nar_hash,
            nar_size: info.nar_size,
            references: info.references,
            content_addressed: info.ca,
        });
    }
    if !entries.is_empty() {
        bail!(
            "nix path-info returned {} unexpected outputs",
            entries.len()
        );
    }
    Ok(result)
}

async fn run_json<T, I, S>(args: I) -> Result<T>
where
    T: DeserializeOwned,
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
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
    if entries.len() != 1 {
        bail!(
            "{command} returned {} entries; expected exactly one",
            entries.len()
        );
    }
    Ok(entries.into_iter().next().expect("length was checked").0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_build_precedes_rebuild_and_disables_substitutes_by_default() {
        assert_eq!(
            initial_build_args("nixpkgs#hello", false),
            [
                "build",
                "--option",
                "substitute",
                "false",
                "nixpkgs#hello",
                "--no-link",
                "--json",
            ]
        );
        assert_eq!(rebuild_args("nixpkgs#hello", false)[1], "--rebuild");
    }

    #[test]
    fn build_arguments_can_enable_substitutes() {
        assert_eq!(initial_build_args("nixpkgs#hello", true)[3], "true");
        assert_eq!(rebuild_args("nixpkgs#hello", true)[4], "true");
    }

    #[test]
    fn build_json_preserves_all_selected_outputs() {
        let outputs = parse_build_outputs(
            r#"[{"outputs":{"bin":"/nix/store/openssl-bin","man":"/nix/store/openssl-man"}}]"#,
        )
        .unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs["man"], "/nix/store/openssl-man");
    }
}
