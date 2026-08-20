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

    /// HTTP(S) Nix binary cache used as the artifact upstream
    #[arg(long, env = "NIX_CACHE_UPSTREAM_URL")]
    pub upstream_cache: Option<String>,

    /// Distinct builders that must agree before a narinfo is published
    #[arg(long, env = "NIX_CACHE_MIN_BUILDERS", default_value_t = 2)]
    pub cache_min_builders: usize,
}
