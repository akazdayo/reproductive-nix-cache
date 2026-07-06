use clap::Parser;
use std::net::SocketAddr;

#[derive(Parser)]
#[command(name = "reproductive-nix-cache-server")]
pub struct Cli {
    /// Address to listen on
    #[arg(long)]
    pub listen: Option<SocketAddr>,
}
