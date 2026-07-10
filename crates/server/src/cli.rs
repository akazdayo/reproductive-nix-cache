use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "reproductive-nix-cache-server")]
pub struct Cli {
    /// Address to listen on
    #[arg(long)]
    pub listen: Option<SocketAddr>,

    /// SQLite database file used to persist build evidence
    #[arg(long)]
    pub database: Option<PathBuf>,
}
