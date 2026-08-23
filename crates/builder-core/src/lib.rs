mod build;
mod client;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use reqwest::StatusCode;
use serde::Serialize;
use shared::{
    BuildCommand, CacheLocation, CommitmentReceipt, CommitmentRequest, Evidence, EvidenceList,
    EvidenceReceipt, EvidenceReveal, Package, RoundPhase, RoundStatus, evidence_commitment,
    generate_nonce,
};
use std::time::Duration;

pub use client::Host;

#[derive(Debug, Clone)]
pub struct BuilderConfig {
    pub builder_id: String,
    pub server: Host,
    pub cache_locations: Vec<String>,
    pub quiet: bool,
}

#[derive(Debug, Serialize)]
pub struct BuildExecution {
    pub evidence: Evidence,
    pub commitment: CommitmentReceipt,
    pub receipt: EvidenceReceipt,
    pub round: RoundStatus,
    /// Uninterpreted evidence returned by the registry.
    pub facts: EvidenceList,
}

pub async fn execute_build(
    command: &BuildCommand,
    config: &BuilderConfig,
) -> Result<BuildExecution> {
    command.validate().map_err(anyhow::Error::msg)?;
    let package = Package::parse_reference(&command.package_ref).map_err(anyhow::Error::msg)?;
    let evidence = build::evidence::generate_evidence(
        package,
        config.builder_id.clone(),
        config.quiet,
        command.substitute,
        command.claims.clone(),
    )
    .await?;
    let registry = client::RegistryClient::new(&config.server)?;
    submit_evidence(&registry, evidence, &config.cache_locations).await
}

async fn submit_evidence(
    registry: &client::RegistryClient,
    evidence: Evidence,
    cache_locations: &[String],
) -> Result<BuildExecution> {
    let derivation_path = evidence
        .build_claim()
        .context("generated evidence has no build claim")?
        .derivation_path
        .clone();

    'round: loop {
        // A new nonce is required for every round. The evidence can be reused
        // without rebuilding, but reusing a revealed nonce would defeat the
        // commit-reveal protocol.
        let nonce = generate_nonce().map_err(anyhow::Error::msg)?;
        let digest = evidence_commitment(&evidence, &nonce).map_err(anyhow::Error::msg)?;
        let commitment = match registry
            .commit(&CommitmentRequest {
                builder_id: evidence.builder_id.clone(),
                derivation_path: derivation_path.clone(),
                digest,
            })
            .await
        {
            Ok(commitment) => commitment,
            Err(error) if error.status() == Some(StatusCode::CONFLICT) => {
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue 'round;
            }
            Err(error) => return Err(error.into()),
        };

        let round_id = commitment.round.id;
        let mut round =
            wait_while_phase(registry, commitment.round.clone(), RoundPhase::Committing).await?;
        if round.phase.is_closed() {
            continue 'round;
        }
        if round.phase != RoundPhase::Revealing {
            bail!("commit-reveal round entered an unsupported phase");
        }

        let reveal = EvidenceReveal {
            round_id,
            nonce,
            evidence: evidence.clone(),
            cache_locations: cache_locations
                .iter()
                .cloned()
                .map(|uri| CacheLocation { uri })
                .collect(),
        };
        let receipt = loop {
            match registry.reveal(&reveal).await {
                Ok(receipt) => break receipt,
                Err(error) if error.status() == Some(StatusCode::GONE) => continue 'round,
                Err(error) if error.status() == Some(StatusCode::CONFLICT) => {
                    round = registry.round_status(round_id).await?;
                    if round.phase.is_closed() {
                        continue 'round;
                    }
                    round = wait_while_phase(registry, round, RoundPhase::Committing).await?;
                    if round.phase.is_closed() {
                        continue 'round;
                    }
                }
                Err(error) => return Err(error.into()),
            }
        };

        round = wait_until_closed(registry, round).await?;
        let facts = registry.round_facts(round_id).await?;
        return Ok(BuildExecution {
            evidence,
            commitment,
            receipt,
            round,
            facts,
        });
    }
}

async fn wait_while_phase(
    registry: &client::RegistryClient,
    mut round: RoundStatus,
    phase: RoundPhase,
) -> Result<RoundStatus> {
    while round.phase == phase {
        tokio::time::sleep(poll_delay(&round)).await;
        round = registry.round_status(round.id).await?;
    }
    Ok(round)
}

async fn wait_until_closed(
    registry: &client::RegistryClient,
    mut round: RoundStatus,
) -> Result<RoundStatus> {
    while !round.phase.is_closed() {
        tokio::time::sleep(poll_delay(&round)).await;
        round = registry.round_status(round.id).await?;
    }
    Ok(round)
}

fn poll_delay(round: &RoundStatus) -> Duration {
    let deadline = match round.phase {
        RoundPhase::Committing => Some(round.commit_deadline),
        RoundPhase::Revealing => round.reveal_deadline,
        RoundPhase::Completed | RoundPhase::Expired => None,
    };
    let remaining = deadline
        .and_then(|deadline| (deadline - Utc::now()).to_std().ok())
        .unwrap_or(Duration::from_millis(1));
    remaining
        .min(Duration::from_millis(250))
        .max(Duration::from_millis(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::StatusCode as AxumStatusCode,
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use chrono::Duration as ChronoDuration;
    use shared::{
        BuildClaim, BuildOutput, BuildStatement, Claim, EVIDENCE_SCHEMA_VERSION, ResolvedSource,
        StoredEvidence,
    };
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    #[test]
    fn polling_never_sleeps_past_a_short_deadline() {
        let round = RoundStatus {
            id: 1,
            derivation_path: "/nix/store/example.drv".into(),
            phase: RoundPhase::Committing,
            commit_count: 1,
            reveal_count: 0,
            commit_deadline: Utc::now() + ChronoDuration::milliseconds(40),
            reveal_deadline: None,
        };
        assert!(poll_delay(&round) <= Duration::from_millis(40));
        assert!(poll_delay(&round) >= Duration::from_millis(1));
    }

    #[derive(Clone)]
    struct RegistryState {
        evidence: Evidence,
        digests: Arc<Mutex<Vec<String>>>,
    }

    fn test_evidence() -> Evidence {
        Evidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            builder_id: "builder-a".into(),
            package: Package {
                repository: "nixpkgs".into(),
                name: "hello".into(),
            },
            claims: vec![Claim::Build(Box::new(BuildClaim {
                source: ResolvedSource {
                    resolved_url: "flake:nixpkgs".into(),
                    revision: None,
                    nar_hash: None,
                },
                derivation_path: "/nix/store/hello.drv".into(),
                build_statement: BuildStatement {
                    outputs: vec![BuildOutput {
                        output_name: "out".into(),
                        output_store_path: "/nix/store/hello".into(),
                        nar_hash: "sha256-output".into(),
                        nar_size: 1,
                        references: vec![],
                        closure_root: "/nix/store/hello".into(),
                        content_addressed: None,
                    }],
                    build_log_digest: None,
                    sbom_digest: None,
                    test_result_digest: None,
                },
                built_at: Utc::now(),
            }))],
        }
    }

    fn round(phase: RoundPhase) -> RoundStatus {
        RoundStatus {
            id: 2,
            derivation_path: "/nix/store/hello.drv".into(),
            phase,
            commit_count: 1,
            reveal_count: usize::from(phase == RoundPhase::Completed),
            commit_deadline: Utc::now() + ChronoDuration::seconds(1),
            reveal_deadline: Some(Utc::now() + ChronoDuration::seconds(1)),
        }
    }

    fn stored(evidence: &Evidence) -> StoredEvidence {
        StoredEvidence {
            id: 42,
            round_id: Some(2),
            evidence: evidence.clone(),
            received_at: Utc::now(),
        }
    }

    async fn commit_handler(
        State(state): State<RegistryState>,
        Json(commitment): Json<CommitmentRequest>,
    ) -> Response {
        let attempt = {
            let mut digests = state.digests.lock().unwrap();
            digests.push(commitment.digest);
            digests.len()
        };
        if attempt == 1 {
            return (
                AxumStatusCode::CONFLICT,
                Json(serde_json::json!({"error":"round is revealing"})),
            )
                .into_response();
        }
        Json(CommitmentReceipt {
            inserted: true,
            round: round(RoundPhase::Revealing),
        })
        .into_response()
    }

    async fn reveal_handler(
        State(state): State<RegistryState>,
        Json(_): Json<EvidenceReveal>,
    ) -> Json<EvidenceReceipt> {
        Json(EvidenceReceipt {
            inserted: true,
            evidence: stored(&state.evidence),
        })
    }

    #[tokio::test]
    async fn late_join_recommits_same_evidence_with_a_fresh_nonce() {
        let evidence = test_evidence();
        let state = RegistryState {
            evidence: evidence.clone(),
            digests: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/v1/evidence/commitments", post(commit_handler))
            .route("/v1/evidence/reveals", post(reveal_handler))
            .route(
                "/v1/evidence/rounds/{round_id}",
                get(|| async { Json(round(RoundPhase::Completed)) }),
            )
            .route(
                "/v1/evidence",
                get({
                    let evidence = evidence.clone();
                    move || {
                        let evidence = evidence.clone();
                        async move {
                            Json(EvidenceList {
                                derivation_path: "/nix/store/hello.drv".into(),
                                evidences: vec![stored(&evidence)],
                            })
                        }
                    }
                }),
            )
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let registry = client::RegistryClient::new(&Host::Ip(address)).unwrap();

        let result = submit_evidence(&registry, evidence, &[]).await.unwrap();

        assert_eq!(result.round.phase, RoundPhase::Completed);
        let digests = state.digests.lock().unwrap();
        assert_eq!(digests.len(), 2);
        assert_ne!(digests[0], digests[1]);
    }
}
