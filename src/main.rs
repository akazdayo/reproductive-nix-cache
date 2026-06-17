mod build_test;
mod models;
mod nix;
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
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build {
            package_ref,
            full_rebuild,
        } => {}
    }
    Ok(())
}
