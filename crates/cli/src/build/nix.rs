use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, de::DeserializeOwned};
use shared::{Package, ResolvedSource};
use std::{collections::BTreeMap, process::Stdio};
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
    let started_at = Utc::now();
    let mut child = Command::new("nix")
        .args(build_args(reference.as_str(), substitute))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start nix build")?;
    let stdout = child.stdout.take().context("nix stdout was not piped")?;
    let stderr = child.stderr.take().context("nix stderr was not piped")?;
    let wait = async move { child.wait().await.context("failed to wait for nix build") };
    let (stdout, stderr, status) = tokio::try_join!(
        // stdout contains the internal `nix build --json` result. Keep it for
        // evidence collection without mixing it into the CLI's final output.
        capture_stream(stdout, tokio::io::stdout(), false),
        capture_stream(stderr, tokio::io::stderr(), !quiet),
        wait,
    )?;
    let finished_at = Utc::now();

    if !status.success() {
        bail!("nix build failed with exit status: {status}");
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

fn build_args(reference: &str, substitute: bool) -> [&str; 8] {
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

pub async fn copy_to_cache(cache_url: &str, output_paths: &[String], quiet: bool) -> Result<()> {
    if output_paths.is_empty() {
        bail!("cannot copy an empty output set to the binary cache");
    }

    let mut child = Command::new("nix")
        .args(copy_args(cache_url, output_paths))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start nix copy")?;
    let stdout = child
        .stdout
        .take()
        .context("nix copy stdout was not piped")?;
    let stderr = child
        .stderr
        .take()
        .context("nix copy stderr was not piped")?;
    let wait = async move { child.wait().await.context("failed to wait for nix copy") };
    let (_, stderr, status) = tokio::try_join!(
        // Keep stdout reserved for the CLI's final human-readable or JSON
        // result. Nix reports copy progress on stderr.
        capture_stream(stdout, tokio::io::stdout(), false),
        capture_stream(stderr, tokio::io::stderr(), !quiet),
        wait,
    )?;

    if !status.success() {
        bail!("nix copy failed with exit status {status}: {stderr}");
    }

    Ok(())
}

fn copy_args(cache_url: &str, output_paths: &[String]) -> Vec<String> {
    let mut args = vec!["copy".into(), "--to".into(), cache_url.into()];
    args.extend(output_paths.iter().cloned());
    args
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
    fn build_args_disable_substitutes_by_default() {
        assert_eq!(
            build_args("nixpkgs#hello", false),
            [
                "build",
                "--rebuild",
                "--option",
                "substitute",
                "false",
                "nixpkgs#hello",
                "--no-link",
                "--json",
            ]
        );
    }

    #[test]
    fn build_args_can_enable_substitutes() {
        assert_eq!(build_args("nixpkgs#hello", true)[4], "true");
    }

    #[test]
    fn build_json_preserves_all_selected_outputs() {
        let outputs = parse_build_outputs(
            r#"[{
                "drvPath": "/nix/store/openssl.drv",
                "outputs": {
                    "bin": "/nix/store/openssl-bin",
                    "man": "/nix/store/openssl-man"
                }
            }]"#,
        )
        .unwrap();

        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs["bin"], "/nix/store/openssl-bin");
        assert_eq!(outputs["man"], "/nix/store/openssl-man");
    }

    #[test]
    fn path_info_is_joined_with_every_built_output() {
        let outputs = BTreeMap::from([
            ("bin".into(), "/nix/store/openssl-bin".into()),
            ("man".into(), "/nix/store/openssl-man".into()),
        ]);
        let entries: BTreeMap<String, NixPathInfo> = serde_json::from_str(
            r#"{
                "/nix/store/openssl-bin": {
                    "narHash": "sha256-bin",
                    "narSize": 1234,
                    "references": ["/nix/store/glibc"],
                    "ca": null
                },
                "/nix/store/openssl-man": {
                    "narHash": "sha256-man",
                    "narSize": 567,
                    "references": [],
                    "ca": "fixed:r:sha256:example"
                }
            }"#,
        )
        .unwrap();
        let info = collect_output_info(&outputs, entries).unwrap();

        assert_eq!(info.len(), 2);
        assert_eq!(info[0].output_name, "bin");
        assert_eq!(info[0].nar_hash, "sha256-bin");
        assert_eq!(info[1].output_name, "man");
        assert_eq!(info[1].nar_size, 567);
        assert_eq!(
            info[1].content_addressed.as_deref(),
            Some("fixed:r:sha256:example")
        );
    }

    #[test]
    fn only_entry_rejects_multiple_nix_results() {
        let entries = BTreeMap::from([("one".into(), ()), ("two".into(), ())]);
        assert!(take_only_entry(entries, "nix path-info").is_err());
    }

    #[test]
    fn copy_args_include_every_output() {
        let outputs = vec![
            "/nix/store/openssl-bin".into(),
            "/nix/store/openssl-man".into(),
        ];

        assert_eq!(
            copy_args(
                "s3://nix-cache?scheme=http&endpoint=127.0.0.1:9000",
                &outputs
            ),
            [
                "copy",
                "--to",
                "s3://nix-cache?scheme=http&endpoint=127.0.0.1:9000",
                "/nix/store/openssl-bin",
                "/nix/store/openssl-man",
            ]
        );
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
