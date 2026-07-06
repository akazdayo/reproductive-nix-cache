mod cli;
mod config;
use axum::{Router, routing::get};
use clap::Parser;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    let app = Router::new().route("/", get(health));

    let config = config::Config::default();
    let listen = cli.listen.unwrap_or(config.listen_addr);
    let listener = TcpListener::bind(listen).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}
