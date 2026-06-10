mod evidence;
mod models;
mod nix;

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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build {
            package_ref,
            full_rebuild,
        } => {
            let evidence = evidence::build_evidence(&package_ref, full_rebuild)?;
            println!("{}", serde_json::to_string_pretty(&evidence)?);
        }
    }
    Ok(())
}
