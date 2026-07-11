use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, de::DeserializeOwned};
use shared::{Package, ResolvedSource};
use std::{collections::BTreeMap, process::Stdio};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    pub output_path: String,
    pub nar_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRun {
    pub stdout: String,
    pub stderr: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
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

pub async fn build(package: &Package, quiet: bool) -> Result<BuildRun> {
    let reference = package.reference();
    let started_at = Utc::now();
    let mut child = Command::new("nix")
        .args([
            "build",
            "--rebuild",
            "--option",
            "substitute",
            "false",
            reference.as_str(),
            "--no-link",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start nix build")?;
    let stdout = child.stdout.take().context("nix stdout was not piped")?;
    let stderr = child.stderr.take().context("nix stderr was not piped")?;
    let wait = async move { child.wait().await.context("failed to wait for nix build") };
    let (stdout, stderr, status) = tokio::try_join!(
        capture_stream(stdout, tokio::io::stdout(), !quiet),
        capture_stream(stderr, tokio::io::stderr(), !quiet),
        wait,
    )?;
    let finished_at = Utc::now();

    if !status.success() {
        bail!("nix build failed with exit status: {status}");
    }

    Ok(BuildRun {
        stdout,
        stderr,
        started_at,
        finished_at,
    })
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn only_entry_rejects_multiple_nix_results() {
        let entries = BTreeMap::from([("one".into(), ()), ("two".into(), ())]);
        assert!(take_only_entry(entries, "nix path-info").is_err());
    }

    #[tokio::test]
    async fn capture_stream_preserves_all_bytes_and_echoes_when_enabled() {
        let (mut source_writer, source_reader) = tokio::io::duplex(64);
        let source = tokio::spawn(async move {
            source_writer.write_all(b"first\nsecond\n").await.unwrap();
        });
        let (mut echo_reader, echo_writer) = tokio::io::duplex(64);

        let captured = capture_stream(source_reader, echo_writer, true)
            .await
            .unwrap();
        source.await.unwrap();
        let mut echoed = String::new();
        echo_reader.read_to_string(&mut echoed).await.unwrap();

        assert_eq!(captured, "first\nsecond\n");
        assert_eq!(echoed, captured);
    }

    #[tokio::test]
    async fn capture_stream_keeps_content_but_does_not_echo_when_quiet() {
        let (mut source_writer, source_reader) = tokio::io::duplex(64);
        let source = tokio::spawn(async move {
            source_writer.write_all(b"build output\n").await.unwrap();
        });
        let (mut echo_reader, echo_writer) = tokio::io::duplex(64);

        let captured = capture_stream(source_reader, echo_writer, false)
            .await
            .unwrap();
        source.await.unwrap();
        let mut echoed = String::new();
        echo_reader.read_to_string(&mut echoed).await.unwrap();

        assert_eq!(captured, "build output\n");
        assert!(echoed.is_empty());
    }
}
