use crate::{
    entity::{build_output, claim, evidence, round},
    store::{EvidenceStore, OverviewSnapshot, RoundConfig},
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use shared::RoundPhase;
use std::collections::BTreeMap;

pub const INDEX_HTML: &str = include_str!("../static/index.html");

#[derive(Serialize)]
pub struct OverviewResponse {
    generated_at: DateTime<Utc>,
    rounds: Vec<OverviewRound>,
}

#[derive(Serialize)]
struct OverviewRound {
    id: i64,
    derivation_path: String,
    phase: RoundPhase,
    started_at: DateTime<Utc>,
    commit_deadline: DateTime<Utc>,
    reveal_deadline: Option<DateTime<Utc>>,
    closed_at: Option<DateTime<Utc>>,
    commit_count: usize,
    reveal_count: usize,
    participants: Vec<OverviewParticipant>,
}

#[derive(Serialize)]
struct OverviewParticipant {
    builder_id: String,
    reveal_status: RevealStatus,
    committed_at: DateTime<Utc>,
    revealed_at: Option<DateTime<Utc>>,
    evidence_id: Option<i64>,
    package: Option<OverviewPackage>,
    outputs: Vec<OverviewOutput>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum RevealStatus {
    Waiting,
    Failed,
    Success,
}

#[derive(Serialize)]
struct OverviewPackage {
    repository: String,
    name: String,
}

#[derive(Serialize)]
struct OverviewOutput {
    name: String,
    store_path: String,
    nar_hash: String,
    nar_size: u64,
}

pub async fn load(store: &EvidenceStore, config: RoundConfig) -> Result<OverviewResponse> {
    let snapshot = store.overview_snapshot(config).await?;
    Ok(response_from_snapshot(snapshot))
}

fn response_from_snapshot(snapshot: OverviewSnapshot) -> OverviewResponse {
    let evidences = snapshot
        .evidences
        .into_iter()
        .map(|evidence| (evidence.id, evidence))
        .collect::<BTreeMap<_, _>>();
    let claims = snapshot
        .claims
        .into_iter()
        .map(|claim| (claim.evidence_id, claim))
        .collect::<BTreeMap<_, _>>();
    let mut outputs = BTreeMap::<i64, Vec<build_output::Model>>::new();
    for output in snapshot.build_outputs {
        outputs.entry(output.claim_id).or_default().push(output);
    }
    let mut commitments = BTreeMap::new();
    for commitment in snapshot.commitments {
        commitments
            .entry(commitment.round_id)
            .or_insert_with(Vec::new)
            .push(commitment);
    }

    OverviewResponse {
        generated_at: Utc::now(),
        rounds: snapshot
            .rounds
            .into_iter()
            .map(|round| {
                let round_id = round.id;
                round_response(
                    round,
                    commitments.remove(&round_id).unwrap_or_default(),
                    &evidences,
                    &claims,
                    &outputs,
                )
            })
            .collect(),
    }
}

fn round_response(
    round: round::Model,
    commitments: Vec<crate::entity::commitment::Model>,
    evidences: &BTreeMap<i64, evidence::Model>,
    claims: &BTreeMap<i64, claim::Model>,
    outputs: &BTreeMap<i64, Vec<build_output::Model>>,
) -> OverviewRound {
    let phase = round_phase(&round);
    let commit_count = commitments.len();
    let participants = commitments
        .into_iter()
        .map(|commitment| {
            let evidence = commitment.evidence_id.and_then(|id| evidences.get(&id));
            let participant_outputs = commitment
                .evidence_id
                .and_then(|id| claims.get(&id))
                .and_then(|claim| outputs.get(&claim.id))
                .into_iter()
                .flatten()
                .map(|output| OverviewOutput {
                    name: output.output_name.clone(),
                    store_path: output.output_store_path.clone(),
                    nar_hash: output.nar_hash.clone(),
                    nar_size: u64::try_from(output.nar_size).unwrap_or_default(),
                })
                .collect();
            OverviewParticipant {
                builder_id: commitment.builder_id,
                reveal_status: if commitment.evidence_id.is_some() {
                    RevealStatus::Success
                } else if phase == RoundPhase::Expired {
                    RevealStatus::Failed
                } else {
                    RevealStatus::Waiting
                },
                committed_at: commitment.committed_at,
                revealed_at: commitment.revealed_at,
                evidence_id: commitment.evidence_id,
                package: evidence.map(|evidence| OverviewPackage {
                    repository: evidence.package_repository.clone(),
                    name: evidence.package_name.clone(),
                }),
                outputs: participant_outputs,
            }
        })
        .collect::<Vec<_>>();
    let reveal_count = participants
        .iter()
        .filter(|participant| matches!(participant.reveal_status, RevealStatus::Success))
        .count();

    OverviewRound {
        id: round.id,
        derivation_path: round.derivation_path,
        phase,
        started_at: round.started_at,
        commit_deadline: round.commit_deadline,
        reveal_deadline: round.reveal_deadline,
        closed_at: round.closed_at,
        commit_count,
        reveal_count,
        participants,
    }
}

fn round_phase(round: &round::Model) -> RoundPhase {
    if round.closed_at.is_some() {
        if round.expired {
            RoundPhase::Expired
        } else {
            RoundPhase::Completed
        }
    } else if round.reveal_started_at.is_some() {
        RoundPhase::Revealing
    } else {
        RoundPhase::Committing
    }
}
