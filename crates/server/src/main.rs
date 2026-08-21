mod api;
mod binary_cache;
mod cache_location;
mod cli;
mod config;
mod entity;
mod round_manager;
mod store;
use anyhow::bail;
use chrono::Duration;
use clap::Parser;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    let config = config::Config::default();
    let listen = cli.listen.unwrap_or(config.listen_addr);
    let database = cli.database.unwrap_or(config.database_path);
    let store = store::EvidenceStore::open(&database).await?;
    if cli.cache_min_builders == 0 || cli.commit_min_builders == 0 {
        bail!("builder thresholds must be at least 1");
    }
    if cli.commit_window_seconds <= 0 || cli.reveal_window_seconds <= 0 {
        bail!("commit and reveal windows must be at least 1 second");
    }
    if cli.commit_min_builders < cli.cache_min_builders {
        bail!("--commit-min-builders must be at least --cache-min-builders");
    }
    let round_config = store::RoundConfig {
        minimum_builders: cli.commit_min_builders,
        commit_window: Duration::seconds(cli.commit_window_seconds),
        reveal_window: Duration::seconds(cli.reveal_window_seconds),
    };
    let cache = binary_cache::BinaryCache::new(cli.cache_min_builders)?;
    let mut state = api::AppState::new(store)
        .with_round_config(round_config)
        .with_binary_cache(cache);
    if !cli.builder_nodes.is_empty() {
        if cli.builder_nodes.len() < cli.commit_min_builders {
            bail!(
                "configured builder nodes must be at least --commit-min-builders ({})",
                cli.commit_min_builders
            );
        }
        let manager_token = cli
            .manager_token
            .ok_or_else(|| anyhow::anyhow!("NIX_CACHE_MANAGER_TOKEN is required"))?;
        let builder_token = cli
            .builder_token
            .ok_or_else(|| anyhow::anyhow!("NIX_CACHE_BUILDER_TOKEN is required"))?;
        let manager = round_manager::RoundManager::new(
            cli.builder_nodes,
            builder_token,
            cli.build_queue_capacity,
        )?;
        state = state.with_round_manager(manager, manager_token);
    }
    let app = api::router(state);
    let listener = TcpListener::bind(listen).await?;
    eprintln!(
        "reproductive-nix-cache server listening on {listen}; database: {}",
        database.display()
    );
    axum::serve(listener, app).await?;

    Ok(())
}
