mod output;
mod trust;

use anyhow::Result;
use builder_core::{BuilderConfig, Host, execute_build};
use clap::{Parser, Subcommand};
use serde::Serialize;
use shared::{
    BuildCommand, ClaimKind, CommitmentReceipt, Evidence, EvidenceList, EvidenceReceipt,
    RoundStatus,
};

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
        /// HTTP(S) Nix binary cache containing the built outputs; may be repeated
        #[arg(long = "cache-location")]
        cache_locations: Vec<String>,
        /// Suppress Nix build output
        #[arg(long)]
        quiet: bool,
        /// Allow Nix to obtain build outputs from substituters
        #[arg(long)]
        substitute: bool,
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
    commitment: CommitmentReceipt,
    receipt: EvidenceReceipt,
    round: RoundStatus,
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
            cache_locations,
            quiet,
            substitute,
            claims,
            json,
        } => {
            let execution = execute_build(
                &BuildCommand {
                    package_ref,
                    substitute,
                    claims,
                },
                &BuilderConfig {
                    builder_id,
                    server,
                    cache_locations,
                    quiet,
                },
            )
            .await?;
            let trust = trust::calculate_trust(&execution.facts);

            let result = BuildResult {
                evidence: execution.evidence,
                commitment: execution.commitment,
                receipt: execution.receipt,
                round: execution.round,
                facts: execution.facts,
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
            ClaimKind::with_required_build(claims),
            vec![ClaimKind::Build]
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
    fn build_accepts_multiple_cache_locations() {
        let cli = Cli::try_parse_from([
            "reproductive-nix-cache",
            "build",
            "nixpkgs#hello",
            "--builder-id",
            "builder-a",
            "--server",
            "127.0.0.1:3000",
            "--cache-location",
            "https://cache-a.example.com/builds",
            "--cache-location",
            "http://cache-b.example.com/",
        ])
        .unwrap();
        let Command::Build {
            cache_locations, ..
        } = cli.command;
        assert_eq!(
            cache_locations,
            vec![
                "https://cache-a.example.com/builds",
                "http://cache-b.example.com/"
            ]
        );
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
        assert_eq!(claims, vec![ClaimKind::Log]);
        assert_eq!(
            ClaimKind::with_required_build(claims),
            vec![ClaimKind::Build, ClaimKind::Log]
        );
    }
}
