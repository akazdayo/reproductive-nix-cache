use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use url::Url;

#[derive(Parser)]
#[command(name = "reproductive-nix-cache-server")]
pub struct Cli {
    /// Address to listen on
    #[arg(long)]
    pub listen: Option<SocketAddr>,

    /// SQLite database file used to persist build evidence
    #[arg(long)]
    pub database: Option<PathBuf>,

    /// Distinct builders that must agree before a narinfo is published
    #[arg(long, env = "NIX_CACHE_MIN_BUILDERS", default_value_t = 2)]
    pub cache_min_builders: usize,

    /// Distinct commitments that close the commit phase
    #[arg(long, env = "NIX_CACHE_COMMIT_MIN_BUILDERS", default_value_t = 2)]
    pub commit_min_builders: usize,

    /// Maximum seconds to accept commitments in a round
    #[arg(long, env = "NIX_CACHE_COMMIT_WINDOW_SECONDS", default_value_t = 60)]
    pub commit_window_seconds: i64,

    /// Maximum seconds to accept reveals in a round
    #[arg(long, env = "NIX_CACHE_REVEAL_WINDOW_SECONDS", default_value_t = 60)]
    pub reveal_window_seconds: i64,

    /// Builder node origins that receive each build command; may be repeated
    #[arg(long = "builder-node")]
    pub builder_nodes: Vec<Url>,

    /// Maximum number of build commands waiting for the round manager worker
    #[arg(long, env = "NIX_CACHE_BUILD_QUEUE_CAPACITY", default_value_t = 64)]
    pub build_queue_capacity: usize,
}
