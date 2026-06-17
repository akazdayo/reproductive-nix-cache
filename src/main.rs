mod build_test;
mod models;
mod nix;
mod utils;

use anyhow::anyhow;
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
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build {
            package_ref,
            full_rebuild,
        } => {
            let stream = nix::run_build(&package_ref, full_rebuild)
                .await
                .map_err(|err| anyhow!("failed to run nix build: {err:?}"))?;
            utils::output_readable_stream(stream).await?;
        }
    }
    Ok(())
}
