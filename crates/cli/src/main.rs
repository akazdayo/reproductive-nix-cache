mod build;
mod client;
mod trust;
mod utils;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use shared::{BuildEvidence, EvidenceList, EvidenceReceipt};

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
        /// Base URL of the evidence registry, for example http://127.0.0.1:3000
        #[arg(long)]
        server: String,
        /// Suppress Nix build output
        #[arg(long)]
        quiet: bool,
    },
}

#[derive(Serialize)]
struct BuildResult {
    evidence: BuildEvidence,
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
        } => {
            let package = utils::parse_nix_repository(&package_ref).with_context(|| {
                "package reference must have the form <flake>#<attribute>, for example nixpkgs#hello"
            })?;
            let evidence = build::evidence::generate_evidence(package, builder_id, quiet).await?;
            let registry = client::RegistryClient::new(server)?;
            let receipt = registry.submit(&evidence).await?;
            let facts = registry.facts(&evidence.derivation_path).await?;
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
