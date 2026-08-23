mod api;
mod binary_cache;
mod cache_location;
mod cli;
mod config;
mod entity;
mod metrics;
mod overview;
mod round_manager;
mod store;
use anyhow::bail;
use chrono::Duration;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();
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
    let builder_node_count = cli.builder_nodes.len();
    let mut state = api::AppState::new(store)
        .with_round_config(round_config)
        .with_binary_cache(cache)
        .with_server_metadata(metrics::ServerMetadata {
            listen: listen.to_string(),
            database: database.display().to_string(),
        });
    if !cli.builder_nodes.is_empty() {
        if cli.builder_nodes.len() < cli.commit_min_builders {
            bail!(
                "configured builder nodes must be at least --commit-min-builders ({})",
                cli.commit_min_builders
            );
        }
        let manager =
            round_manager::RoundManager::new(cli.builder_nodes, cli.build_queue_capacity)?;
        state = state.with_round_manager(manager);
    }
    let app = api::router(state);
    let listener = TcpListener::bind(listen).await?;
    info!(
        %listen,
        database = %database.display(),
        commit_min_builders = cli.commit_min_builders,
        cache_min_builders = cli.cache_min_builders,
        commit_window_seconds = cli.commit_window_seconds,
        reveal_window_seconds = cli.reveal_window_seconds,
        builder_nodes = builder_node_count,
        "server started"
    );
    axum::serve(listener, app).await?;

    Ok(())
}
