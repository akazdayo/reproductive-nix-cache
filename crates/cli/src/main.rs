mod build;
mod claims;
mod client;
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
        /// Additional claim to include; the build claim is always included
        #[arg(long = "claim", value_enum)]
        claims: Vec<ClaimKind>,
    },
}

#[derive(Serialize)]
struct BuildResult {
    evidence: Evidence,
    receipt: EvidenceReceipt,
    /// Uninterpreted evidence returned by the registry.
    facts: EvidenceList,
    /// Calculated locally from `facts`; it is never supplied by the registry.
    trust: trust::TrustScore,
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
            claims,
        } => {
            let package = utils::parse_nix_repository(&package_ref).with_context(|| {
                "package reference must have the form <flake>#<attribute>, for example nixpkgs#hello"
            })?;
            let enabled_claims = ClaimKind::with_required_build(claims);
            let evidence =
                build::evidence::generate_evidence(package, builder_id, quiet, enabled_claims)
                    .await?;
            let registry = client::RegistryClient::new(&server)?;
            let receipt = registry.submit(&evidence).await?;
            let derivation_path = evidence
                .build_claim()
                .context("generated evidence has no build claim")?
                .derivation_path
                .clone();
            let facts = registry.facts(&derivation_path).await?;
            let trust = trust::calculate_trust(&facts);

            println!(
                "{}",
                serde_json::to_string_pretty(&BuildResult {
                    evidence,
                    receipt,
                    facts,
                    trust,
                })?
            );
        }
    }
    Ok(())
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
        let Command::Build { claims, .. } = cli.command;
        assert!(claims.is_empty());
        assert_eq!(
            crate::claims::ClaimKind::with_required_build(claims),
            vec![crate::claims::ClaimKind::Build]
        );
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
