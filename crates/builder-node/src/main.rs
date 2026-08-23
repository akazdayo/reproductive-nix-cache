use anyhow::{Context, bail};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use builder_core::{BuilderConfig, Host, execute_build};
use clap::Parser;
use serde::Serialize;
use shared::{BuildCommand, BuildNodeReceipt};
use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc};
use tokio::{net::TcpListener, sync::Semaphore};

type BuildFuture = Pin<Box<dyn Future<Output = Result<BuildNodeReceipt, String>> + Send + 'static>>;
type BuildExecutor = Arc<dyn Fn(BuildCommand) -> BuildFuture + Send + Sync>;

#[derive(Parser)]
#[command(name = "reproductive-nix-cache-builder-node")]
struct Cli {
    /// Address to listen on
    #[arg(long, default_value = "[::]:51338")]
    listen: SocketAddr,

    /// Stable identifier for this independent builder
    #[arg(long)]
    builder_id: String,

    /// Evidence registry host
    #[arg(long)]
    server: Host,

    /// HTTP(S) Nix binary cache containing this node's outputs; may be repeated
    #[arg(long = "cache-location")]
    cache_locations: Vec<String>,

    /// Suppress Nix build output
    #[arg(long)]
    quiet: bool,
}

#[derive(Clone)]
struct AppState {
    permits: Arc<Semaphore>,
    execute: BuildExecutor,
}

fn router(execute: BuildExecutor) -> Router {
    Router::new()
        .route("/", get(health))
        .route("/v1/builds", post(run_build))
        .with_state(AppState {
            permits: Arc::new(Semaphore::new(1)),
            execute,
        })
}

async fn health() -> &'static str {
    "ok"
}

async fn run_build(State(state): State<AppState>, Json(command): Json<BuildCommand>) -> Response {
    if let Err(message) = command.validate() {
        return error(StatusCode::BAD_REQUEST, message);
    }
    let Ok(_permit) = state.permits.clone().try_acquire_owned() else {
        return error(
            StatusCode::CONFLICT,
            "builder node is already running a build",
        );
    };

    match (state.execute)(command).await {
        Ok(receipt) => (StatusCode::OK, Json(receipt)).into_response(),
        Err(message) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
        .into_response()
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if cli.builder_id.trim().is_empty() {
        bail!("--builder-id must not be empty");
    }
    let config = Arc::new(BuilderConfig {
        builder_id: cli.builder_id,
        server: cli.server,
        cache_locations: cli.cache_locations,
        quiet: cli.quiet,
    });
    let execute: BuildExecutor = Arc::new(move |command| {
        let config = Arc::clone(&config);
        Box::pin(async move {
            let result = execute_build(&command, &config).await.map_err(|error| {
                eprintln!("builder node build failed: {error:#}");
                "build failed; see builder node logs".to_owned()
            })?;
            Ok(BuildNodeReceipt {
                builder_id: result.evidence.builder_id.clone(),
                round_id: result.round.id,
                evidence_id: result.receipt.evidence.id,
            })
        })
    });
    let app = router(execute);
    let listener = TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("failed to listen on {}", cli.listen))?;
    eprintln!(
        "reproductive-nix-cache builder node listening on {}",
        cli.listen
    );
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use std::sync::Mutex;
    use tower::ServiceExt;

    fn request(command: &BuildCommand) -> Request<Body> {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/builds")
            .header(header::CONTENT_TYPE, "application/json");
        request
            .body(Body::from(serde_json::to_vec(command).unwrap()))
            .unwrap()
    }

    fn command() -> BuildCommand {
        BuildCommand {
            package_ref: "nixpkgs#hello".into(),
            substitute: false,
            claims: vec![],
        }
    }

    #[tokio::test]
    async fn rejects_invalid_commands_before_execution() {
        let execute: BuildExecutor = Arc::new(|_| panic!("must not execute"));
        let app = router(execute);

        let invalid = BuildCommand {
            package_ref: "invalid".into(),
            ..command()
        };
        assert_eq!(
            app.oneshot(request(&invalid)).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn executes_the_exact_command_and_returns_receipt() {
        let received = Arc::new(Mutex::new(None));
        let capture = Arc::clone(&received);
        let execute: BuildExecutor = Arc::new(move |command| {
            *capture.lock().unwrap() = Some(command);
            Box::pin(async {
                Ok(BuildNodeReceipt {
                    builder_id: "builder-a".into(),
                    round_id: 7,
                    evidence_id: 42,
                })
            })
        });
        let app = router(execute);
        let command = command();
        let response = app.oneshot(request(&command)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let receipt: BuildNodeReceipt =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(receipt.evidence_id, 42);
        assert_eq!(*received.lock().unwrap(), Some(command));
    }

    #[tokio::test]
    async fn rejects_a_second_build_while_one_is_running() {
        let execute: BuildExecutor = Arc::new(|_| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok(BuildNodeReceipt {
                    builder_id: "builder-a".into(),
                    round_id: 1,
                    evidence_id: 1,
                })
            })
        });
        let app = router(execute);
        let first_app = app.clone();
        let first =
            tokio::spawn(async move { first_app.oneshot(request(&command())).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let second = app.oneshot(request(&command())).await.unwrap();
        assert_eq!(second.status(), StatusCode::CONFLICT);
        assert_eq!(first.await.unwrap().status(), StatusCode::OK);
    }
}
