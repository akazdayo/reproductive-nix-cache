use crate::store::{EvidenceStore, validate};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use shared::{BuildEvidence, EvidenceList};

#[derive(Clone)]
pub struct AppState {
    store: EvidenceStore,
}

impl AppState {
    pub fn new(store: EvidenceStore) -> Self {
        Self { store }
    }

    #[cfg(test)]
    async fn in_memory() -> anyhow::Result<Self> {
        Ok(Self::new(EvidenceStore::in_memory().await?))
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(health))
        .route("/v1/evidence", post(submit_evidence).get(list_evidence))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn submit_evidence(
    State(state): State<AppState>,
    Json(evidence): Json<BuildEvidence>,
) -> ApiResult<impl IntoResponse> {
    validate(&evidence).map_err(ApiError::bad_request)?;
    let receipt = state
        .store
        .insert(&evidence)
        .await
        .map_err(ApiError::internal)?;
    let status = if receipt.inserted {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };

    Ok((status, Json(receipt)))
}

#[derive(Debug, Deserialize)]
struct EvidenceQuery {
    derivation_path: Option<String>,
}

async fn list_evidence(
    State(state): State<AppState>,
    Query(query): Query<EvidenceQuery>,
) -> ApiResult<Json<EvidenceList>> {
    let derivation_path = query
        .derivation_path
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("derivation_path query parameter is required"))?;
    let evidence = state
        .store
        .list(&derivation_path)
        .await
        .map_err(ApiError::internal)?;

    Ok(Json(evidence))
}

type ApiResult<T> = Result<T, ApiError>;

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.to_string(),
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use chrono::Utc;
    use shared::{EVIDENCE_SCHEMA_VERSION, EvidenceReceipt, Package, ResolvedSource};
    use tower::ServiceExt;

    fn evidence(builder_id: &str, nar_hash: &str) -> BuildEvidence {
        BuildEvidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            builder_id: builder_id.into(),
            package: Package {
                repository: "nixpkgs".into(),
                name: "hello".into(),
            },
            source: ResolvedSource {
                resolved_url: "flake:nixpkgs".into(),
                revision: None,
                nar_hash: Some("sha256-source".into()),
            },
            derivation_path: "/nix/store/example-hello.drv".into(),
            output_path: "/nix/store/example-hello".into(),
            nar_hash: nar_hash.into(),
            built_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn api_persists_and_returns_raw_evidence_facts() {
        let app = router(AppState::in_memory().await.unwrap());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/evidence")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&evidence("builder-a", "sha256-out")).unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let receipt_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(receipt_json.get("score").is_none());
        assert!(receipt_json.get("trust").is_none());
        let receipt: EvidenceReceipt = serde_json::from_slice(&body).unwrap();
        assert!(receipt.inserted);

        let duplicate = Request::builder()
            .method("POST")
            .uri("/v1/evidence")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&evidence("builder-a", "sha256-out")).unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(duplicate).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let duplicate_receipt: EvidenceReceipt =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(!duplicate_receipt.inserted);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/evidence?derivation_path=%2Fnix%2Fstore%2Fexample-hello.drv")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let facts_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(facts_json.get("score").is_none());
        assert!(facts_json.get("trust").is_none());
        let facts: EvidenceList = serde_json::from_slice(&body).unwrap();
        assert_eq!(facts.evidences.len(), 1);
        assert_eq!(facts.evidences[0].evidence.builder_id, "builder-a");
        assert_eq!(facts.evidences[0].evidence.nar_hash, "sha256-out");
    }
}
