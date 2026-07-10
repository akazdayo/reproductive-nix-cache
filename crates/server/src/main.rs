mod api;
mod cli;
mod config;
mod entity;
mod store;
use clap::Parser;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    let config = config::Config::default();
    let listen = cli.listen.unwrap_or(config.listen_addr);
    let database = cli.database.unwrap_or(config.database_path);
    let store = store::EvidenceStore::open(&database).await?;
    let app = api::router(api::AppState::new(store));
    let listener = TcpListener::bind(listen).await?;
    eprintln!(
        "reproductive-nix-cache server listening on {listen}; database: {}",
        database.display()
    );
    axum::serve(listener, app).await?;

    Ok(())
}
