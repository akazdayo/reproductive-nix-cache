mod build;
mod claims;
mod utils;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "reproductive-nix-cache")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Build {
        /// Package reference like nixpkgs#hello
        package_ref: String,
        /// Rebuild all dependencies from scratch (no cache, no substitutes)
        #[arg(long)]
        full_rebuild: bool,
        #[arg(long)]
        quiet: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build {
            package_ref,
            full_rebuild,
            quiet,
        } => match utils::parse_nix_repository(&package_ref) {
            Some(pkg) => {
                let result = build::evidence::generate_evidence(
                    pkg,
                    vec![shared::Evidences::Logs],
                    full_rebuild,
                    quiet,
                )
                .await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            None => {
                panic!("Parse Error!")
            }
        },
    }
    Ok(())
}
