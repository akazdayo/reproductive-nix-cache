mod build;
mod claims;
mod client;
mod output;
mod trust;
mod utils;

use anyhow::{Context, Result};
use claims::ClaimKind;
use clap::{Parser, Subcommand};
use client::Host;
use serde::Serialize;
use shared::{Evidence, EvidenceList, EvidenceReceipt};

#[derive(Parser)]
#[command(name = "reproductive-nix-cache")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Rebuild a package locally and register its evidence with a registry.
    Build {
        /// Package reference like nixpkgs#hello
        package_ref: String,
        /// Stable identifier for this independent builder
        #[arg(long)]
        builder_id: String,
        /// Host of the evidence registry, for example 127.0.0.1:3000 or example.com
        #[arg(long)]
        server: Host,
        /// Suppress Nix build output
        #[arg(long)]
        quiet: bool,
        /// Allow Nix to obtain build outputs from substituters
        #[arg(long)]
        substitute: bool,
        /// S3 binary cache receiving the build outputs
        #[arg(
            long,
            env = "NIX_CACHE_S3_URL",
            value_parser = parse_binary_cache_url
        )]
        binary_cache: Option<String>,
        /// Additional claim to include; the build claim is always included
        #[arg(long = "claim", value_enum)]
        claims: Vec<ClaimKind>,
        /// Print the complete result as JSON instead of a human-readable summary
        #[arg(long)]
        json: bool,
    },
}

#[derive(Serialize)]
struct BuildResult {
    evidence: Evidence,
    cache: Option<CacheUpload>,
    receipt: EvidenceReceipt,
    /// Uninterpreted evidence returned by the registry.
    facts: EvidenceList,
    /// Calculated locally from `facts`; it is never supplied by the registry.
    trust: trust::TrustScore,
}

#[derive(Serialize)]
struct CacheUpload {
    store_url: String,
    output_paths: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build {
            package_ref,
            builder_id,
            server,
            quiet,
            substitute,
            binary_cache,
            claims,
            json,
        } => {
            let package = utils::parse_nix_repository(&package_ref).with_context(|| {
                "package reference must have the form <flake>#<attribute>, for example nixpkgs#hello"
            })?;
            let enabled_claims = ClaimKind::with_required_build(claims);
            let evidence = build::evidence::generate_evidence(
                package,
                builder_id,
                quiet,
                substitute,
                enabled_claims,
            )
            .await?;
            let cache = if let Some(store_url) = binary_cache {
                let output_paths = evidence
                    .build_claim()
                    .context("generated evidence has no build claim")?
                    .build_statement
                    .outputs
                    .iter()
                    .map(|output| output.output_store_path.clone())
                    .collect::<Vec<_>>();
                build::nix::copy_to_cache(&store_url, &output_paths, quiet).await?;
                Some(CacheUpload {
                    store_url,
                    output_paths,
                })
            } else {
                None
            };
            let registry = client::RegistryClient::new(&server)?;
            let receipt = registry.submit(&evidence).await?;
            let derivation_path = evidence
                .build_claim()
                .context("generated evidence has no build claim")?
                .derivation_path
                .clone();
            let facts = registry.facts(&derivation_path).await?;
            let trust = trust::calculate_trust(&facts);

            let result = BuildResult {
                evidence,
                cache,
                receipt,
                facts,
                trust,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("{}", output::format_build_summary(&result));
            }
        }
    }
    Ok(())
}

fn parse_binary_cache_url(value: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(value).map_err(|error| error.to_string())?;
    if url.scheme() != "s3" {
        return Err("binary cache URL must use the s3 scheme".into());
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("binary cache URL must include a bucket name".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("binary cache URL must not contain credentials".into());
    }

    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_defaults_to_required_build_claim_only() {
        let cli = Cli::try_parse_from([
            "reproductive-nix-cache",
            "build",
            "nixpkgs#hello",
            "--builder-id",
            "builder-a",
            "--server",
            "127.0.0.1:3000",
        ])
        .unwrap();
        let Command::Build {
            claims, substitute, ..
        } = cli.command;
        assert!(claims.is_empty());
        assert!(!substitute);
        assert_eq!(
            crate::claims::ClaimKind::with_required_build(claims),
            vec![crate::claims::ClaimKind::Build]
        );
    }

    #[test]
    fn build_accepts_substitute_option() {
        let cli = Cli::try_parse_from([
            "reproductive-nix-cache",
            "build",
            "nixpkgs#hello",
            "--builder-id",
            "builder-a",
            "--server",
            "127.0.0.1:3000",
            "--substitute",
        ])
        .unwrap();
        let Command::Build { substitute, .. } = cli.command;
        assert!(substitute);
    }

    #[test]
    fn build_accepts_s3_binary_cache() {
        let cli = Cli::try_parse_from([
            "reproductive-nix-cache",
            "build",
            "nixpkgs#hello",
            "--builder-id",
            "builder-a",
            "--server",
            "127.0.0.1:3000",
            "--binary-cache",
            "s3://nix-cache?scheme=http&endpoint=127.0.0.1:9000",
        ])
        .unwrap();
        let Command::Build { binary_cache, .. } = cli.command;

        assert_eq!(
            binary_cache.as_deref(),
            Some("s3://nix-cache?scheme=http&endpoint=127.0.0.1:9000")
        );
    }

    #[test]
    fn build_rejects_non_s3_binary_cache() {
        let error = Cli::try_parse_from([
            "reproductive-nix-cache",
            "build",
            "nixpkgs#hello",
            "--builder-id",
            "builder-a",
            "--server",
            "127.0.0.1:3000",
            "--binary-cache",
            "http://127.0.0.1:9000/nix-cache",
        ])
        .err()
        .expect("non-S3 cache URL should be rejected");

        assert!(error.to_string().contains("must use the s3 scheme"));
    }

    #[test]
    fn build_accepts_json_output_option() {
        let cli = Cli::try_parse_from([
            "reproductive-nix-cache",
            "build",
            "nixpkgs#hello",
            "--builder-id",
            "builder-a",
            "--server",
            "127.0.0.1:3000",
            "--json",
        ])
        .unwrap();
        let Command::Build { json, .. } = cli.command;
        assert!(json);
    }

    #[test]
    fn build_accepts_log_as_an_optional_claim() {
        let cli = Cli::try_parse_from([
            "reproductive-nix-cache",
            "build",
            "nixpkgs#hello",
            "--builder-id",
            "builder-a",
            "--server",
            "127.0.0.1:3000",
            "--claim",
            "log",
        ])
        .unwrap();
        let Command::Build { claims, .. } = cli.command;
        assert_eq!(claims, vec![crate::claims::ClaimKind::Log]);
        assert_eq!(
            crate::claims::ClaimKind::with_required_build(claims),
            vec![
                crate::claims::ClaimKind::Build,
                crate::claims::ClaimKind::Log
            ]
        );
    }
}
