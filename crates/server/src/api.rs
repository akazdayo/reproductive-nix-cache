use crate::{
    binary_cache::{BinaryCache, CACHE_INFO},
    store::{EvidenceStore, validate},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use shared::{Evidence, EvidenceList};

#[derive(Clone)]
pub struct AppState {
    store: EvidenceStore,
    binary_cache: Option<BinaryCache>,
}

impl AppState {
    pub fn new(store: EvidenceStore) -> Self {
        Self {
            store,
            binary_cache: None,
        }
    }

    pub fn with_binary_cache(mut self, binary_cache: BinaryCache) -> Self {
        self.binary_cache = Some(binary_cache);
        self
    }

    #[cfg(test)]
    async fn in_memory() -> anyhow::Result<Self> {
        Ok(Self::new(EvidenceStore::in_memory().await?))
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(health))
        .route(
            "/v1/evidence",
            post(submit_evidence)
                .get(list_evidence)
                .layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/nix-cache-info",
            get(binary_cache_info).head(binary_cache_info_head),
        )
        .route("/nar/{store_hash}", get(get_nar).head(head_nar))
        .route("/{key}", get(get_narinfo).head(head_narinfo))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn submit_evidence(
    State(state): State<AppState>,
    Json(evidence): Json<Evidence>,
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

async fn binary_cache_info(State(state): State<AppState>) -> Response {
    if state.binary_cache.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/x-nix-cache-info")],
        CACHE_INFO,
    )
        .into_response()
}

async fn binary_cache_info_head(State(state): State<AppState>) -> Response {
    if state.binary_cache.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/x-nix-cache-info")
        .header(header::CONTENT_LENGTH, CACHE_INFO.len())
        .body(Body::empty())
        .unwrap()
}

async fn get_narinfo(State(state): State<AppState>, Path(key): Path<String>) -> Response {
    narinfo_response(&state, &key, false).await
}

async fn head_narinfo(State(state): State<AppState>, Path(key): Path<String>) -> Response {
    narinfo_response(&state, &key, true).await
}

async fn narinfo_response(state: &AppState, key: &str, head_only: bool) -> Response {
    let Some(cache) = &state.binary_cache else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match cache.approved_narinfo(&state.store, key).await {
        Ok(Some(approved)) => {
            let length = approved.bytes.len();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/x-nix-narinfo")
                .header(header::CONTENT_LENGTH, length)
                .body(if head_only {
                    Body::empty()
                } else {
                    Body::from(approved.bytes)
                })
                .unwrap()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => ApiError::bad_gateway(error).into_response(),
    }
}

async fn get_nar(State(state): State<AppState>, Path(store_hash): Path<String>) -> Response {
    let Some(cache) = &state.binary_cache else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match cache.get_nar(&state.store, &store_hash).await {
        Ok(Some(result)) => {
            let length = result.headers().get(header::CONTENT_LENGTH).cloned();
            let content_type = result.headers().get(header::CONTENT_TYPE).cloned();
            let mut response = Response::builder().status(StatusCode::OK);
            if let Some(length) = length {
                response = response.header(header::CONTENT_LENGTH, length);
            }
            if let Some(content_type) = content_type {
                response = response.header(header::CONTENT_TYPE, content_type);
            }
            response
                .body(Body::from_stream(result.bytes_stream()))
                .unwrap()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => ApiError::bad_gateway(error).into_response(),
    }
}

async fn head_nar(State(state): State<AppState>, Path(store_hash): Path<String>) -> Response {
    let Some(cache) = &state.binary_cache else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match cache.head_nar(&state.store, &store_hash).await {
        Ok(Some(result)) => {
            let mut response = Response::builder().status(StatusCode::OK);
            if let Some(length) = result.headers().get(header::CONTENT_LENGTH) {
                response = response.header(header::CONTENT_LENGTH, length);
            }
            if let Some(content_type) = result.headers().get(header::CONTENT_TYPE) {
                response = response.header(header::CONTENT_TYPE, content_type);
            }
            response.body(Body::empty()).unwrap()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => ApiError::bad_gateway(error).into_response(),
    }
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

    fn bad_gateway(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
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
    use chrono::{Duration, Utc};
    use shared::{
        BuildClaim, BuildOutput, BuildStatement, Claim, EVIDENCE_SCHEMA_VERSION, Evidence,
        EvidenceReceipt, LogClaim, Package, ResolvedSource,
    };
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    const CACHE_HASH: &str = "00000000000000000000000000000000";
    const CACHE_STORE_PATH: &str = "/nix/store/00000000000000000000000000000000-hello";
    const CACHE_NAR_HASH_NIX32: &str =
        "sha256:0f3gg73cybjfnzlav06r5ndr4711wv2gjkgk2s0lghp2h3cy6db7";
    const CACHE_NAR_HASH_SRI: &str = "sha256-ZzXj2YDiwkeBFvNN+cTmIRySmy3ZgK3ot04uz8Z5bzg=";
    const OTHER_NAR_HASH_SRI: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn evidence(builder_id: &str, nar_hash: &str) -> Evidence {
        let started_at = Utc::now();
        let finished_at = started_at + Duration::seconds(1);
        Evidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            builder_id: builder_id.into(),
            package: Package {
                repository: "nixpkgs".into(),
                name: "hello".into(),
            },
            claims: vec![
                Claim::Build(Box::new(BuildClaim {
                    source: ResolvedSource {
                        resolved_url: "flake:nixpkgs".into(),
                        revision: None,
                        nar_hash: Some("sha256-source".into()),
                    },
                    derivation_path: "/nix/store/example-hello.drv".into(),
                    build_statement: BuildStatement {
                        outputs: vec![BuildOutput {
                            output_name: "out".into(),
                            output_store_path: "/nix/store/example-hello".into(),
                            nar_hash: nar_hash.into(),
                            nar_size: 1234,
                            references: vec!["/nix/store/glibc".into()],
                            closure_root: "/nix/store/example-hello".into(),
                            content_addressed: None,
                        }],
                        build_log_digest: None,
                        sbom_digest: None,
                        test_result_digest: None,
                    },
                    built_at: finished_at,
                })),
                Claim::Log(LogClaim {
                    stdout: "stdout\n".into(),
                    stderr: "stderr\n".into(),
                    started_at,
                    finished_at,
                }),
            ],
        }
    }

    fn cache_evidence(builder_id: &str, nar_hash: &str) -> Evidence {
        let mut evidence = evidence(builder_id, nar_hash);
        let build = evidence
            .claims
            .iter_mut()
            .find_map(|claim| match claim {
                Claim::Build(build) => Some(build),
                Claim::Log(_) => None,
            })
            .unwrap();
        let output = &mut build.build_statement.outputs[0];
        output.output_store_path = CACHE_STORE_PATH.into();
        output.closure_root = CACHE_STORE_PATH.into();
        evidence
    }

    fn narinfo(nar_hash: &str) -> String {
        format!(
            "StorePath: {CACHE_STORE_PATH}\n\
             URL: nar/example.nar.xz\n\
             Compression: xz\n\
             FileHash: sha256-file\n\
             FileSize: 99\n\
             NarHash: {nar_hash}\n\
             NarSize: 1234\n\
             References: glibc\n"
        )
    }

    async fn spawn_upstream(narinfo: String) -> reqwest::Url {
        let narinfo_route = narinfo.clone();
        let upstream = Router::new()
            .route(
                "/{key}",
                get(move |Path(key): Path<String>| {
                    let narinfo = narinfo_route.clone();
                    async move {
                        if key == format!("{CACHE_HASH}.narinfo") {
                            ([(header::CONTENT_TYPE, "text/x-nix-narinfo")], narinfo)
                                .into_response()
                        } else {
                            StatusCode::NOT_FOUND.into_response()
                        }
                    }
                }),
            )
            .route(
                "/nar/example.nar.xz",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/x-nix-nar")],
                        "compressed nar",
                    )
                })
                .head(|| async {
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "application/x-nix-nar")
                        .header(header::CONTENT_LENGTH, 14)
                        .body(Body::empty())
                        .unwrap()
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        reqwest::Url::parse(&format!("http://{address}/")).unwrap()
    }

    async fn cache_app_with_narinfo(
        minimum_builders: usize,
        evidence: Vec<Evidence>,
        contents: String,
    ) -> Router {
        let store = EvidenceStore::in_memory().await.unwrap();
        for item in evidence {
            store.insert(&item).await.unwrap();
        }
        let upstream = spawn_upstream(contents).await;
        let state = AppState::new(store)
            .with_binary_cache(BinaryCache::for_tests(upstream, minimum_builders));
        router(state)
    }

    async fn cache_app(minimum_builders: usize, evidence: Vec<Evidence>) -> Router {
        cache_app_with_narinfo(minimum_builders, evidence, narinfo(CACHE_NAR_HASH_NIX32)).await
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
        assert_eq!(response.status(), StatusCode::CREATED);
        let duplicate_receipt: EvidenceReceipt =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(duplicate_receipt.inserted);

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
        assert_eq!(facts.evidences.len(), 2);
        assert_eq!(facts.evidences[0].evidence.builder_id, "builder-a");
        assert_eq!(
            facts.evidences[0]
                .evidence
                .build_claim()
                .unwrap()
                .build_statement
                .outputs[0]
                .nar_hash,
            "sha256-out"
        );
    }

    #[tokio::test]
    async fn api_rejects_missing_or_multiple_build_claims() {
        let app = router(AppState::in_memory().await.unwrap());
        let mut missing = evidence("builder-a", "sha256-out");
        missing
            .claims
            .retain(|claim| !matches!(claim, Claim::Build(_)));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/evidence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&missing).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let mut multiple = evidence("builder-a", "sha256-out");
        multiple.claims.push(multiple.claims[0].clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/evidence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&multiple).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn api_accepts_log_claim_larger_than_two_megabytes() {
        let app = router(AppState::in_memory().await.unwrap());
        let mut evidence = evidence("builder-a", "sha256-out");
        let Claim::Log(log) = &mut evidence.claims[1] else {
            unreachable!()
        };
        log.stdout = "x".repeat(2 * 1024 * 1024 + 1);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/evidence")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&evidence).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn cache_publishes_narinfo_only_after_builder_consensus() {
        let unapproved = cache_app(2, vec![cache_evidence("builder-a", CACHE_NAR_HASH_SRI)]).await;
        let request = Request::builder()
            .uri(format!("/{CACHE_HASH}.narinfo"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            unapproved.oneshot(request).await.unwrap().status(),
            StatusCode::NOT_FOUND
        );

        let approved = cache_app(
            2,
            vec![
                cache_evidence("builder-a", CACHE_NAR_HASH_SRI),
                cache_evidence("builder-b", CACHE_NAR_HASH_SRI),
            ],
        )
        .await;
        let response = approved
            .oneshot(
                Request::builder()
                    .uri(format!("/{CACHE_HASH}.narinfo"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            narinfo(CACHE_NAR_HASH_NIX32)
                .replace("URL: nar/example.nar.xz", &format!("URL: nar/{CACHE_HASH}"),)
        );
    }

    #[tokio::test]
    async fn cache_rejects_narinfo_that_differs_from_consensus() {
        let app = cache_app(
            2,
            vec![
                cache_evidence("builder-a", OTHER_NAR_HASH_SRI),
                cache_evidence("builder-b", OTHER_NAR_HASH_SRI),
            ],
        )
        .await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/{CACHE_HASH}.narinfo"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cache_hides_tied_variants_and_missing_upstream_entries() {
        let app = cache_app(
            1,
            vec![
                cache_evidence("builder-a", CACHE_NAR_HASH_SRI),
                cache_evidence("builder-b", OTHER_NAR_HASH_SRI),
            ],
        )
        .await;
        let tied = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/{CACHE_HASH}.narinfo"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(tied.status(), StatusCode::NOT_FOUND);

        let missing = app
            .oneshot(
                Request::builder()
                    .uri("/11111111111111111111111111111111.narinfo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cache_streams_nars_and_supports_head() {
        let app = cache_app(1, vec![cache_evidence("builder-a", CACHE_NAR_HASH_SRI)]).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/nar/{CACHE_HASH}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "compressed nar"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri(format!("/nar/{CACHE_HASH}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "14");
        assert!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/nar/example.nar.xz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cache_returns_bad_gateway_for_invalid_or_unsafe_narinfo() {
        let approved = vec![cache_evidence("builder-a", CACHE_NAR_HASH_SRI)];
        let invalid = cache_app_with_narinfo(
            1,
            approved.clone(),
            narinfo(CACHE_NAR_HASH_NIX32).replace("NarSize: 1234", "NarSize: invalid"),
        )
        .await;
        let response = invalid
            .oneshot(
                Request::builder()
                    .uri(format!("/{CACHE_HASH}.narinfo"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let unsafe_url = cache_app_with_narinfo(
            1,
            approved,
            narinfo(CACHE_NAR_HASH_NIX32).replace(
                "URL: nar/example.nar.xz",
                "URL: https://evil.example/example.nar.xz",
            ),
        )
        .await;
        let response = unsafe_url
            .oneshot(
                Request::builder()
                    .uri(format!("/{CACHE_HASH}.narinfo"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn cache_returns_not_found_when_an_approved_nar_is_missing_upstream() {
        let app = cache_app_with_narinfo(
            1,
            vec![cache_evidence("builder-a", CACHE_NAR_HASH_SRI)],
            narinfo(CACHE_NAR_HASH_NIX32)
                .replace("URL: nar/example.nar.xz", "URL: nar/missing.nar.xz"),
        )
        .await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/nar/{CACHE_HASH}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cache_does_not_follow_upstream_redirects() {
        let upstream = Router::new().route(
            &format!("/{CACHE_HASH}.narinfo"),
            get(|| async {
                Response::builder()
                    .status(StatusCode::TEMPORARY_REDIRECT)
                    .header(header::LOCATION, "/redirected.narinfo")
                    .body(Body::empty())
                    .unwrap()
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

        let store = EvidenceStore::in_memory().await.unwrap();
        store
            .insert(&cache_evidence("builder-a", CACHE_NAR_HASH_SRI))
            .await
            .unwrap();
        let cache = BinaryCache::for_tests(
            reqwest::Url::parse(&format!("http://{address}/")).unwrap(),
            1,
        );
        let app = router(AppState::new(store).with_binary_cache(cache));
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/{CACHE_HASH}.narinfo"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn cache_info_is_available_only_when_cache_is_configured() {
        let disabled = router(AppState::in_memory().await.unwrap());
        let response = disabled
            .oneshot(
                Request::builder()
                    .uri("/nix-cache-info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let enabled = cache_app(1, vec![]).await;
        let response = enabled
            .oneshot(
                Request::builder()
                    .uri("/nix-cache-info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            CACHE_INFO
        );
    }
}
