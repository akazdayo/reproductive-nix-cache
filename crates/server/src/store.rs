use crate::entity::{
    build_claim, build_output as build_output_entity, claim, commitment,
    evidence as evidence_entity, log_claim, round,
};
use anyhow::{Context, Result, bail};
use chrono::{Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
    DatabaseConnection, EntityTrait, ModelTrait, PaginatorTrait, QueryFilter, QueryOrder, Schema,
    Set, Statement, TransactionTrait, sea_query::Index,
};
use shared::{
    BuildClaim, BuildOutput, BuildStatement, Claim, CommitmentReceipt, CommitmentRequest, Evidence,
    EvidenceList, EvidenceReceipt, EvidenceReveal, LogClaim, Package, ResolvedSource, RoundPhase,
    RoundStatus, StoredEvidence, verify_evidence_commitment,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const BUILD_KIND: &str = "build";
const LOG_KIND: &str = "log";

#[derive(Debug, Clone, Copy)]
pub struct RoundConfig {
    pub minimum_builders: usize,
    pub commit_window: Duration,
    pub reveal_window: Duration,
}

impl Default for RoundConfig {
    fn default() -> Self {
        Self {
            minimum_builders: 2,
            commit_window: Duration::seconds(60),
            reveal_window: Duration::seconds(60),
        }
    }
}

#[derive(Debug)]
pub enum ProtocolError {
    BadRequest(String),
    NotFound(String),
    Conflict(String),
    Gone(String),
    Internal(anyhow::Error),
}

impl ProtocolError {
    fn internal(error: impl Into<anyhow::Error>) -> Self {
        Self::Internal(error.into())
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadRequest(message)
            | Self::NotFound(message)
            | Self::Conflict(message)
            | Self::Gone(message) => formatter.write_str(message),
            Self::Internal(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutputFingerprint {
    pub nar_hash: String,
    pub nar_size: u64,
    pub references: Vec<String>,
}

impl OutputFingerprint {
    fn from_model(output: &build_output_entity::Model) -> Result<Self> {
        let references: Vec<String> = serde_json::from_str(&output.references_json)
            .context("stored build references are invalid")?;
        Ok(Self {
            nar_hash: output.nar_hash.clone(),
            nar_size: u64::try_from(output.nar_size).context("stored NAR size is negative")?,
            references: normalize_references(references),
        })
    }
}

#[derive(Clone)]
pub struct EvidenceStore {
    database: DatabaseConnection,
}

impl EvidenceStore {
    pub async fn open(path: &Path) -> Result<Self> {
        let absolute_path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()
                .context("failed to resolve current directory")?
                .join(path)
        };
        let database_url = format!("sqlite://{}?mode=rwc", absolute_path.display());
        Self::connect(&database_url, 5).await.with_context(|| {
            format!(
                "failed to open SQLite database at {}",
                absolute_path.display()
            )
        })
    }

    #[cfg(test)]
    pub async fn in_memory() -> Result<Self> {
        Self::connect("sqlite::memory:", 1)
            .await
            .context("failed to open in-memory test database")
    }

    async fn connect(database_url: &str, max_connections: u32) -> Result<Self> {
        let mut options = ConnectOptions::new(database_url.to_owned());
        options
            .max_connections(max_connections)
            .min_connections(1)
            .sqlx_logging(false);
        let database = Database::connect(options)
            .await
            .context("failed to connect through SeaORM")?;
        let store = Self { database };
        store.initialise().await?;
        Ok(store)
    }

    async fn initialise(&self) -> Result<()> {
        self.database
            .execute_unprepared("PRAGMA journal_mode = WAL")
            .await
            .context("failed to enable SQLite WAL mode")?;

        let backend = self.database.get_database_backend();
        let legacy = self
            .database
            .query_one(Statement::from_string(
                backend,
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'evidence'",
            ))
            .await
            .context("failed to inspect SQLite schema")?;
        if legacy.is_some() {
            bail!(
                "legacy evidence schema detected; delete the SQLite database and restart the server"
            );
        }

        let old_build_claims = self
            .database
            .query_one(Statement::from_string(
                backend,
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'build_claims' \
                 AND NOT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = \
                 'build_outputs')",
            ))
            .await
            .context("failed to inspect build statement schema")?;
        if old_build_claims.is_some() {
            bail!(
                "evidence schema predates version 4; delete the SQLite database and restart the server"
            );
        }

        let schema = Schema::new(backend);
        for entity in [
            schema.create_table_from_entity(round::Entity),
            schema.create_table_from_entity(evidence_entity::Entity),
            schema.create_table_from_entity(commitment::Entity),
            schema.create_table_from_entity(claim::Entity),
            schema.create_table_from_entity(build_claim::Entity),
            schema.create_table_from_entity(build_output_entity::Entity),
            schema.create_table_from_entity(log_claim::Entity),
        ] {
            let mut entity = entity;
            entity.if_not_exists();
            self.database
                .execute(backend.build(&entity))
                .await
                .context("failed to create normalized evidence schema")?;
        }

        let mut position_index = Index::create();
        position_index
            .name("claims_evidence_position_uidx")
            .table(claim::Entity)
            .col(claim::Column::EvidenceId)
            .col(claim::Column::Position)
            .unique()
            .if_not_exists();
        self.database
            .execute(backend.build(&position_index))
            .await
            .context("failed to create claim position index")?;

        let mut derivation_index = Index::create();
        derivation_index
            .name("build_claims_derivation_path_idx")
            .table(build_claim::Entity)
            .col(build_claim::Column::DerivationPath)
            .col(build_claim::Column::ClaimId)
            .if_not_exists();
        self.database
            .execute(backend.build(&derivation_index))
            .await
            .context("failed to create build claim lookup index")?;

        let mut output_store_path_index = Index::create();
        output_store_path_index
            .name("build_outputs_store_path_idx")
            .table(build_output_entity::Entity)
            .col(build_output_entity::Column::OutputStorePath)
            .col(build_output_entity::Column::ClaimId)
            .if_not_exists();
        self.database
            .execute(backend.build(&output_store_path_index))
            .await
            .context("failed to create build output lookup index")?;

        self.database
            .execute_unprepared(
                "CREATE UNIQUE INDEX IF NOT EXISTS rounds_open_derivation_uidx \
                 ON commit_reveal_rounds (derivation_path) WHERE closed_at IS NULL",
            )
            .await
            .context("failed to create active round index")?;
        self.database
            .execute_unprepared(
                "CREATE UNIQUE INDEX IF NOT EXISTS commitments_round_builder_uidx \
                 ON evidence_commitments (round_id, builder_id)",
            )
            .await
            .context("failed to create round builder index")?;
        self.database
            .execute_unprepared(
                "CREATE UNIQUE INDEX IF NOT EXISTS commitments_evidence_uidx \
                 ON evidence_commitments (evidence_id) WHERE evidence_id IS NOT NULL",
            )
            .await
            .context("failed to create revealed evidence index")?;

        Ok(())
    }

    pub async fn commit(
        &self,
        request: &CommitmentRequest,
        config: RoundConfig,
    ) -> std::result::Result<CommitmentReceipt, ProtocolError> {
        validate_commitment_request(request)?;
        let now = Utc::now();

        if let Some(existing) = commitment::Entity::find()
            .filter(commitment::Column::BuilderId.eq(&request.builder_id))
            .filter(commitment::Column::Digest.eq(&request.digest))
            .order_by_desc(commitment::Column::Id)
            .one(&self.database)
            .await
            .map_err(ProtocolError::internal)?
        {
            let existing_round = existing
                .find_related(round::Entity)
                .one(&self.database)
                .await
                .map_err(ProtocolError::internal)?
                .ok_or_else(|| {
                    ProtocolError::internal(anyhow::anyhow!("commitment round is missing"))
                })?;
            if existing_round.derivation_path == request.derivation_path {
                let status = self.round_status_at(existing_round, now, config).await?;
                return Ok(CommitmentReceipt {
                    inserted: false,
                    round: status,
                });
            }
        }

        let active = round::Entity::find()
            .filter(round::Column::DerivationPath.eq(&request.derivation_path))
            .filter(round::Column::ClosedAt.is_null())
            .one(&self.database)
            .await
            .map_err(ProtocolError::internal)?;
        let active = match active {
            Some(active) => self.advance_round(active, now, config).await?,
            None => None,
        };

        let active = if let Some(active) = active {
            if active.reveal_started_at.is_some() {
                return Err(ProtocolError::Conflict(
                    "the active round is already revealing evidence".into(),
                ));
            }
            if let Some(existing) = commitment::Entity::find()
                .filter(commitment::Column::RoundId.eq(active.id))
                .filter(commitment::Column::BuilderId.eq(&request.builder_id))
                .one(&self.database)
                .await
                .map_err(ProtocolError::internal)?
            {
                if existing.digest == request.digest {
                    return Ok(CommitmentReceipt {
                        inserted: false,
                        round: self.round_status_model(&active).await?,
                    });
                }
                return Err(ProtocolError::Conflict(
                    "builder already committed a different digest in this round".into(),
                ));
            }
            active
        } else {
            let insert = round::Entity::insert(round::ActiveModel {
                id: NotSet,
                derivation_path: Set(request.derivation_path.clone()),
                started_at: Set(now),
                commit_deadline: Set(now + config.commit_window),
                reveal_started_at: Set(None),
                reveal_deadline: Set(None),
                closed_at: Set(None),
                expired: Set(false),
            })
            .exec(&self.database)
            .await;
            match insert {
                Ok(result) => round::Entity::find_by_id(result.last_insert_id)
                    .one(&self.database)
                    .await
                    .map_err(ProtocolError::internal)?
                    .ok_or_else(|| {
                        ProtocolError::internal(anyhow::anyhow!("created round was not found"))
                    })?,
                Err(_) => round::Entity::find()
                    .filter(round::Column::DerivationPath.eq(&request.derivation_path))
                    .filter(round::Column::ClosedAt.is_null())
                    .one(&self.database)
                    .await
                    .map_err(ProtocolError::internal)?
                    .ok_or_else(|| {
                        ProtocolError::internal(anyhow::anyhow!("concurrent round creation failed"))
                    })?,
            }
        };

        if active.reveal_started_at.is_some() {
            return Err(ProtocolError::Conflict(
                "the active round is already revealing evidence".into(),
            ));
        }
        let inserted = commitment::Entity::insert(commitment::ActiveModel {
            id: NotSet,
            round_id: Set(active.id),
            builder_id: Set(request.builder_id.clone()),
            digest: Set(request.digest.clone()),
            committed_at: Set(now),
            nonce: Set(None),
            evidence_id: Set(None),
            revealed_at: Set(None),
        })
        .exec(&self.database)
        .await;
        if inserted.is_err() {
            let existing = commitment::Entity::find()
                .filter(commitment::Column::RoundId.eq(active.id))
                .filter(commitment::Column::BuilderId.eq(&request.builder_id))
                .one(&self.database)
                .await
                .map_err(ProtocolError::internal)?
                .ok_or_else(|| {
                    ProtocolError::internal(anyhow::anyhow!(
                        "concurrent commitment creation failed"
                    ))
                })?;
            if existing.digest != request.digest {
                return Err(ProtocolError::Conflict(
                    "builder already committed a different digest in this round".into(),
                ));
            }
            return Ok(CommitmentReceipt {
                inserted: false,
                round: self.round_status_model(&active).await?,
            });
        }

        let count = commitment::Entity::find()
            .filter(commitment::Column::RoundId.eq(active.id))
            .count(&self.database)
            .await
            .map_err(ProtocolError::internal)?;
        let active = if usize::try_from(count).unwrap_or(usize::MAX) >= config.minimum_builders {
            let mut update: round::ActiveModel = active.into();
            update.reveal_started_at = Set(Some(now));
            update.reveal_deadline = Set(Some(now + config.reveal_window));
            update
                .update(&self.database)
                .await
                .map_err(ProtocolError::internal)?
        } else {
            active
        };

        Ok(CommitmentReceipt {
            inserted: true,
            round: self.round_status_model(&active).await?,
        })
    }

    pub async fn round_status(
        &self,
        round_id: i64,
        config: RoundConfig,
    ) -> std::result::Result<RoundStatus, ProtocolError> {
        let model = round::Entity::find_by_id(round_id)
            .one(&self.database)
            .await
            .map_err(ProtocolError::internal)?
            .ok_or_else(|| ProtocolError::NotFound("round was not found".into()))?;
        self.round_status_at(model, Utc::now(), config).await
    }

    pub async fn reveal(
        &self,
        reveal: &EvidenceReveal,
        config: RoundConfig,
    ) -> std::result::Result<EvidenceReceipt, ProtocolError> {
        reveal
            .evidence
            .validate()
            .map_err(ProtocolError::BadRequest)?;
        let derivation_path = reveal
            .evidence
            .build_claim()
            .ok_or_else(|| ProtocolError::BadRequest("evidence has no build claim".into()))?
            .derivation_path
            .clone();
        let now = Utc::now();
        let model = round::Entity::find_by_id(reveal.round_id)
            .one(&self.database)
            .await
            .map_err(ProtocolError::internal)?
            .ok_or_else(|| ProtocolError::NotFound("round was not found".into()))?;
        if model.derivation_path != derivation_path {
            return Err(ProtocolError::Conflict(
                "evidence derivation path does not match the committed round".into(),
            ));
        }

        let committed = commitment::Entity::find()
            .filter(commitment::Column::RoundId.eq(reveal.round_id))
            .filter(commitment::Column::BuilderId.eq(&reveal.evidence.builder_id))
            .one(&self.database)
            .await
            .map_err(ProtocolError::internal)?
            .ok_or_else(|| {
                ProtocolError::Conflict("builder did not commit in this round".into())
            })?;
        verify_evidence_commitment(&reveal.evidence, &reveal.nonce, &committed.digest)
            .map_err(ProtocolError::BadRequest)?;

        if let Some(evidence_id) = committed.evidence_id {
            let evidence = self
                .find_by_id(evidence_id)
                .await
                .map_err(ProtocolError::internal)?
                .ok_or_else(|| {
                    ProtocolError::internal(anyhow::anyhow!("revealed evidence is missing"))
                })?;
            return Ok(EvidenceReceipt {
                inserted: false,
                evidence,
            });
        }
        let model = self
            .advance_round(model, now, config)
            .await?
            .ok_or_else(|| ProtocolError::Gone("the reveal deadline has passed".into()))?;
        if model.reveal_started_at.is_none() {
            return Err(ProtocolError::Conflict(
                "the round is still accepting commitments".into(),
            ));
        }

        let transaction = self
            .database
            .begin()
            .await
            .map_err(ProtocolError::internal)?;
        let committed = commitment::Entity::find_by_id(committed.id)
            .one(&transaction)
            .await
            .map_err(ProtocolError::internal)?
            .ok_or_else(|| {
                ProtocolError::internal(anyhow::anyhow!("commitment disappeared before reveal"))
            })?;
        if let Some(evidence_id) = committed.evidence_id {
            transaction
                .commit()
                .await
                .map_err(ProtocolError::internal)?;
            let evidence = self
                .find_by_id(evidence_id)
                .await
                .map_err(ProtocolError::internal)?
                .ok_or_else(|| {
                    ProtocolError::internal(anyhow::anyhow!("revealed evidence is missing"))
                })?;
            return Ok(EvidenceReceipt {
                inserted: false,
                evidence,
            });
        }
        let evidence_id = insert_evidence(&transaction, &reveal.evidence, now)
            .await
            .map_err(ProtocolError::internal)?;
        let mut update: commitment::ActiveModel = committed.into();
        update.nonce = Set(Some(reveal.nonce.clone()));
        update.evidence_id = Set(Some(evidence_id));
        update.revealed_at = Set(Some(now));
        update
            .update(&transaction)
            .await
            .map_err(ProtocolError::internal)?;
        transaction
            .commit()
            .await
            .map_err(ProtocolError::internal)?;

        let model = round::Entity::find_by_id(reveal.round_id)
            .one(&self.database)
            .await
            .map_err(ProtocolError::internal)?
            .ok_or_else(|| ProtocolError::NotFound("round was not found".into()))?;
        self.advance_round(model, now, config).await?;
        let evidence = self
            .find_by_id(evidence_id)
            .await
            .map_err(ProtocolError::internal)?
            .ok_or_else(|| {
                ProtocolError::internal(anyhow::anyhow!("inserted evidence was not found"))
            })?;
        Ok(EvidenceReceipt {
            inserted: true,
            evidence,
        })
    }

    async fn round_status_at(
        &self,
        model: round::Model,
        now: chrono::DateTime<Utc>,
        config: RoundConfig,
    ) -> std::result::Result<RoundStatus, ProtocolError> {
        let round_id = model.id;
        let model = match self.advance_round(model, now, config).await? {
            Some(model) => model,
            None => round::Entity::find_by_id(round_id)
                .one(&self.database)
                .await
                .map_err(ProtocolError::internal)?
                .ok_or_else(|| ProtocolError::NotFound("round was not found".into()))?,
        };
        self.round_status_model(&model).await
    }

    async fn advance_round(
        &self,
        model: round::Model,
        now: chrono::DateTime<Utc>,
        config: RoundConfig,
    ) -> std::result::Result<Option<round::Model>, ProtocolError> {
        if model.closed_at.is_some() {
            return Ok(None);
        }
        let commit_count = commitment::Entity::find()
            .filter(commitment::Column::RoundId.eq(model.id))
            .count(&self.database)
            .await
            .map_err(ProtocolError::internal)?;
        let reveal_count = commitment::Entity::find()
            .filter(commitment::Column::RoundId.eq(model.id))
            .filter(commitment::Column::EvidenceId.is_not_null())
            .count(&self.database)
            .await
            .map_err(ProtocolError::internal)?;

        let model = if model.reveal_started_at.is_none() && now >= model.commit_deadline {
            let mut update: round::ActiveModel = model.clone().into();
            update.reveal_started_at = Set(Some(model.commit_deadline));
            update.reveal_deadline = Set(Some(model.commit_deadline + config.reveal_window));
            update
                .update(&self.database)
                .await
                .map_err(ProtocolError::internal)?
        } else {
            model
        };
        if model.reveal_started_at.is_some() {
            let all_revealed = commit_count != 0 && commit_count == reveal_count;
            let deadline = model.reveal_deadline.ok_or_else(|| {
                ProtocolError::internal(anyhow::anyhow!("revealing round has no reveal deadline"))
            })?;
            if all_revealed || now >= deadline {
                let mut update: round::ActiveModel = model.clone().into();
                update.closed_at = Set(Some(if all_revealed { now } else { deadline }));
                update.expired = Set(!all_revealed);
                update
                    .update(&self.database)
                    .await
                    .map_err(ProtocolError::internal)?;
                return Ok(None);
            }
        }
        Ok(Some(model))
    }

    async fn round_status_model(
        &self,
        model: &round::Model,
    ) -> std::result::Result<RoundStatus, ProtocolError> {
        let commit_count = commitment::Entity::find()
            .filter(commitment::Column::RoundId.eq(model.id))
            .count(&self.database)
            .await
            .map_err(ProtocolError::internal)?;
        let reveal_count = commitment::Entity::find()
            .filter(commitment::Column::RoundId.eq(model.id))
            .filter(commitment::Column::EvidenceId.is_not_null())
            .count(&self.database)
            .await
            .map_err(ProtocolError::internal)?;
        let phase = if model.closed_at.is_some() {
            if model.expired {
                RoundPhase::Expired
            } else {
                RoundPhase::Completed
            }
        } else if model.reveal_started_at.is_some() {
            RoundPhase::Revealing
        } else {
            RoundPhase::Committing
        };
        Ok(RoundStatus {
            id: model.id,
            derivation_path: model.derivation_path.clone(),
            phase,
            commit_count: usize::try_from(commit_count).unwrap_or(usize::MAX),
            reveal_count: usize::try_from(reveal_count).unwrap_or(usize::MAX),
            commit_deadline: model.commit_deadline,
            reveal_deadline: model.reveal_deadline,
        })
    }

    #[cfg(test)]
    pub async fn insert(&self, evidence: &Evidence) -> Result<EvidenceReceipt> {
        validate(evidence)?;
        let transaction = self.database.begin().await?;
        let evidence_id = insert_evidence(&transaction, evidence, Utc::now()).await?;
        transaction.commit().await?;
        let stored = self
            .find_by_id(evidence_id)
            .await?
            .context("inserted evidence was not found")?;
        Ok(EvidenceReceipt {
            inserted: true,
            evidence: stored,
        })
    }

    pub async fn list(&self, derivation_path: &str) -> Result<EvidenceList> {
        let build_rows = build_claim::Entity::find()
            .filter(build_claim::Column::DerivationPath.eq(derivation_path))
            .find_also_related(claim::Entity)
            .all(&self.database)
            .await
            .context("failed to query build claims")?;
        let evidence_ids = build_rows
            .into_iter()
            .filter_map(|(_, claim)| claim.map(|claim| claim.evidence_id))
            .collect::<Vec<_>>();
        if evidence_ids.is_empty() {
            return Ok(EvidenceList {
                derivation_path: derivation_path.to_owned(),
                evidences: Vec::new(),
            });
        }

        let envelopes = evidence_entity::Entity::find()
            .filter(evidence_entity::Column::Id.is_in(evidence_ids))
            .order_by_asc(evidence_entity::Column::Id)
            .all(&self.database)
            .await
            .context("failed to query evidence envelopes")?;
        let mut evidences = Vec::with_capacity(envelopes.len());
        for envelope in envelopes {
            evidences.push(self.hydrate(envelope).await?);
        }

        Ok(EvidenceList {
            derivation_path: derivation_path.to_owned(),
            evidences,
        })
    }

    pub async fn list_round(&self, round_id: i64) -> Result<Option<EvidenceList>> {
        let Some(round) = round::Entity::find_by_id(round_id)
            .one(&self.database)
            .await
            .context("failed to query commit-reveal round")?
        else {
            return Ok(None);
        };
        let evidence_ids = commitment::Entity::find()
            .filter(commitment::Column::RoundId.eq(round_id))
            .filter(commitment::Column::EvidenceId.is_not_null())
            .order_by_asc(commitment::Column::Id)
            .all(&self.database)
            .await
            .context("failed to query revealed commitments")?
            .into_iter()
            .filter_map(|commitment| commitment.evidence_id)
            .collect::<Vec<_>>();
        let envelopes = if evidence_ids.is_empty() {
            Vec::new()
        } else {
            evidence_entity::Entity::find()
                .filter(evidence_entity::Column::Id.is_in(evidence_ids))
                .order_by_asc(evidence_entity::Column::Id)
                .all(&self.database)
                .await
                .context("failed to query round evidence")?
        };
        let mut evidences = Vec::with_capacity(envelopes.len());
        for envelope in envelopes {
            evidences.push(self.hydrate(envelope).await?);
        }
        Ok(Some(EvidenceList {
            derivation_path: round.derivation_path,
            evidences,
        }))
    }

    pub async fn output_consensus(
        &self,
        store_path: &str,
        minimum_builders: usize,
        config: RoundConfig,
    ) -> Result<Option<OutputFingerprint>> {
        let output_rows = build_output_entity::Entity::find()
            .filter(build_output_entity::Column::OutputStorePath.eq(store_path))
            .all(&self.database)
            .await
            .context("failed to query build output evidence")?;
        let mut facts = Vec::new();
        let mut round_ids = BTreeSet::new();
        for output in output_rows {
            let claim = claim::Entity::find_by_id(output.claim_id)
                .one(&self.database)
                .await?
                .context("build output claim envelope is missing")?;
            let evidence = evidence_entity::Entity::find_by_id(claim.evidence_id)
                .one(&self.database)
                .await?
                .context("build output evidence envelope is missing")?;
            let Some(commitment) = commitment::Entity::find()
                .filter(commitment::Column::EvidenceId.eq(evidence.id))
                .one(&self.database)
                .await
                .context("failed to query output commitment")?
            else {
                continue;
            };
            round_ids.insert(commitment.round_id);
            facts.push((output, evidence.builder_id, commitment.round_id));
        }

        let mut latest: Option<(chrono::DateTime<Utc>, i64)> = None;
        for round_id in round_ids {
            let Some(model) = round::Entity::find_by_id(round_id)
                .one(&self.database)
                .await
                .context("failed to query output round")?
            else {
                continue;
            };
            let model = match self
                .advance_round(model, Utc::now(), config)
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?
            {
                Some(model) => model,
                None => round::Entity::find_by_id(round_id)
                    .one(&self.database)
                    .await
                    .context("failed to reload output round")?
                    .context("output round disappeared")?,
            };
            let Some(closed_at) = model.closed_at else {
                continue;
            };
            let reveals = commitment::Entity::find()
                .filter(commitment::Column::RoundId.eq(round_id))
                .filter(commitment::Column::EvidenceId.is_not_null())
                .count(&self.database)
                .await
                .context("failed to count round reveals")?;
            if usize::try_from(reveals).unwrap_or(usize::MAX) < minimum_builders {
                continue;
            }
            let candidate = (closed_at, round_id);
            if latest.is_none_or(|current| candidate > current) {
                latest = Some(candidate);
            }
        }
        let Some((_, latest_round_id)) = latest else {
            return Ok(None);
        };

        let mut variants: BTreeMap<OutputFingerprint, BTreeSet<String>> = BTreeMap::new();
        for (output, builder_id, round_id) in facts {
            if round_id != latest_round_id {
                continue;
            }
            variants
                .entry(OutputFingerprint::from_model(&output)?)
                .or_default()
                .insert(builder_id);
        }

        let Some(maximum) = variants.values().map(BTreeSet::len).max() else {
            return Ok(None);
        };
        let mut leaders = variants
            .into_iter()
            .filter(|(_, builders)| builders.len() == maximum);
        let Some((fingerprint, builders)) = leaders.next() else {
            return Ok(None);
        };
        if leaders.next().is_some() || builders.len() < minimum_builders {
            return Ok(None);
        }

        Ok(Some(fingerprint))
    }

    async fn find_by_id(&self, id: i64) -> Result<Option<StoredEvidence>> {
        let envelope = evidence_entity::Entity::find_by_id(id)
            .one(&self.database)
            .await
            .context("failed to read evidence envelope")?;
        match envelope {
            Some(envelope) => Ok(Some(self.hydrate(envelope).await?)),
            None => Ok(None),
        }
    }

    async fn hydrate(&self, envelope: evidence_entity::Model) -> Result<StoredEvidence> {
        let round_id = commitment::Entity::find()
            .filter(commitment::Column::EvidenceId.eq(envelope.id))
            .one(&self.database)
            .await
            .context("failed to query evidence commitment")?
            .map(|commitment| commitment.round_id);
        let claim_rows = claim::Entity::find()
            .filter(claim::Column::EvidenceId.eq(envelope.id))
            .order_by_asc(claim::Column::Position)
            .all(&self.database)
            .await
            .context("failed to query claim envelopes")?;
        let mut claims = Vec::with_capacity(claim_rows.len());
        for row in claim_rows {
            let item = match row.kind.as_str() {
                BUILD_KIND => {
                    let model = build_claim::Entity::find_by_id(row.id)
                        .one(&self.database)
                        .await?
                        .context("build claim payload is missing")?;
                    let output_models = build_output_entity::Entity::find()
                        .filter(build_output_entity::Column::ClaimId.eq(row.id))
                        .order_by_asc(build_output_entity::Column::Position)
                        .all(&self.database)
                        .await
                        .context("failed to query build outputs")?;
                    let outputs = output_models
                        .into_iter()
                        .map(|output| {
                            Ok(BuildOutput {
                                output_name: output.output_name,
                                output_store_path: output.output_store_path,
                                nar_hash: output.nar_hash,
                                nar_size: u64::try_from(output.nar_size)
                                    .context("stored NAR size is negative")?,
                                references: serde_json::from_str(&output.references_json)
                                    .context("stored build references are invalid")?,
                                closure_root: output.closure_root,
                                content_addressed: output.content_addressed,
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Claim::Build(Box::new(BuildClaim {
                        source: ResolvedSource {
                            resolved_url: model.source_resolved_url,
                            revision: model.source_revision,
                            nar_hash: model.source_nar_hash,
                        },
                        derivation_path: model.derivation_path,
                        build_statement: BuildStatement {
                            outputs,
                            build_log_digest: model.build_log_digest,
                            sbom_digest: model.sbom_digest,
                            test_result_digest: model.test_result_digest,
                        },
                        built_at: model.built_at,
                    }))
                }
                LOG_KIND => {
                    let model = log_claim::Entity::find_by_id(row.id)
                        .one(&self.database)
                        .await?
                        .context("log claim payload is missing")?;
                    Claim::Log(LogClaim {
                        stdout: model.stdout,
                        stderr: model.stderr,
                        started_at: model.started_at,
                        finished_at: model.finished_at,
                    })
                }
                unknown => bail!("unknown stored claim kind: {unknown}"),
            };
            claims.push(item);
        }

        Ok(StoredEvidence {
            id: envelope.id,
            round_id,
            evidence: Evidence {
                schema_version: u32::try_from(envelope.schema_version)
                    .context("stored schema version is outside the u32 range")?,
                builder_id: envelope.builder_id,
                package: Package {
                    repository: envelope.package_repository,
                    name: envelope.package_name,
                },
                claims,
            },
            received_at: envelope.received_at,
        })
    }
}

async fn insert_evidence<C>(
    connection: &C,
    evidence: &Evidence,
    received_at: chrono::DateTime<Utc>,
) -> Result<i64>
where
    C: ConnectionTrait,
{
    let evidence_result = evidence_entity::Entity::insert(evidence_entity::ActiveModel {
        id: NotSet,
        schema_version: Set(i64::from(evidence.schema_version)),
        builder_id: Set(evidence.builder_id.clone()),
        package_repository: Set(evidence.package.repository.clone()),
        package_name: Set(evidence.package.name.clone()),
        received_at: Set(received_at),
    })
    .exec(connection)
    .await
    .context("failed to store evidence envelope")?;

    for (position, item) in evidence.claims.iter().enumerate() {
        let kind = match item {
            Claim::Build(_) => BUILD_KIND,
            Claim::Log(_) => LOG_KIND,
        };
        let claim_result = claim::Entity::insert(claim::ActiveModel {
            id: NotSet,
            evidence_id: Set(evidence_result.last_insert_id),
            position: Set(i64::try_from(position).context("too many claims")?),
            kind: Set(kind.into()),
        })
        .exec(connection)
        .await
        .context("failed to store claim envelope")?;

        match item {
            Claim::Build(build) => {
                build_claim::Entity::insert(build_claim::ActiveModel {
                    claim_id: Set(claim_result.last_insert_id),
                    source_resolved_url: Set(build.source.resolved_url.clone()),
                    source_revision: Set(build.source.revision.clone()),
                    source_nar_hash: Set(build.source.nar_hash.clone()),
                    derivation_path: Set(build.derivation_path.clone()),
                    build_log_digest: Set(build.build_statement.build_log_digest.clone()),
                    sbom_digest: Set(build.build_statement.sbom_digest.clone()),
                    test_result_digest: Set(build.build_statement.test_result_digest.clone()),
                    built_at: Set(build.built_at),
                })
                .exec(connection)
                .await
                .context("failed to store build claim")?;

                for (position, output) in build.build_statement.outputs.iter().enumerate() {
                    build_output_entity::Entity::insert(build_output_entity::ActiveModel {
                        claim_id: Set(claim_result.last_insert_id),
                        position: Set(i64::try_from(position).context("too many build outputs")?),
                        output_name: Set(output.output_name.clone()),
                        output_store_path: Set(output.output_store_path.clone()),
                        nar_hash: Set(output.nar_hash.clone()),
                        nar_size: Set(i64::try_from(output.nar_size)
                            .context("NAR size exceeds SQLite's signed integer range")?),
                        references_json: Set(serde_json::to_string(&output.references)
                            .context("failed to serialize build references")?),
                        closure_root: Set(output.closure_root.clone()),
                        content_addressed: Set(output.content_addressed.clone()),
                    })
                    .exec(connection)
                    .await
                    .context("failed to store build output")?;
                }
            }
            Claim::Log(log) => {
                log_claim::Entity::insert(log_claim::ActiveModel {
                    claim_id: Set(claim_result.last_insert_id),
                    stdout: Set(log.stdout.clone()),
                    stderr: Set(log.stderr.clone()),
                    started_at: Set(log.started_at),
                    finished_at: Set(log.finished_at),
                })
                .exec(connection)
                .await
                .context("failed to store log claim")?;
            }
        }
    }

    Ok(evidence_result.last_insert_id)
}

fn validate_commitment_request(
    request: &CommitmentRequest,
) -> std::result::Result<(), ProtocolError> {
    if request.builder_id.trim().is_empty() || request.builder_id.len() > 128 {
        return Err(ProtocolError::BadRequest(
            "builder_id must contain between 1 and 128 bytes".into(),
        ));
    }
    if request.derivation_path.trim().is_empty() || request.derivation_path.len() > 4096 {
        return Err(ProtocolError::BadRequest(
            "derivation_path must contain between 1 and 4096 bytes".into(),
        ));
    }
    shared::commitment::validate_digest(&request.digest).map_err(ProtocolError::BadRequest)
}

#[cfg(test)]
pub fn validate(evidence: &Evidence) -> Result<()> {
    evidence.validate().map_err(anyhow::Error::msg)
}

fn normalize_references(references: Vec<String>) -> Vec<String> {
    let mut references = references
        .into_iter()
        .map(|reference| {
            reference
                .rsplit('/')
                .next()
                .unwrap_or(&reference)
                .to_owned()
        })
        .collect::<Vec<_>>();
    references.sort();
    references.dedup();
    references
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use sea_orm::PaginatorTrait;

    fn evidence(nar_hash: &str) -> Evidence {
        let started_at = Utc::now();
        let finished_at = started_at + Duration::seconds(1);
        Evidence {
            schema_version: shared::EVIDENCE_SCHEMA_VERSION,
            builder_id: "builder-a".into(),
            package: Package {
                repository: "nixpkgs".into(),
                name: "hello".into(),
            },
            claims: vec![
                Claim::Log(LogClaim {
                    stdout: "stdout\n".into(),
                    stderr: "stderr\n".into(),
                    started_at,
                    finished_at,
                }),
                Claim::Build(Box::new(BuildClaim {
                    source: ResolvedSource {
                        resolved_url: "flake:nixpkgs".into(),
                        revision: None,
                        nar_hash: Some("sha256-source".into()),
                    },
                    derivation_path: "/nix/store/hello.drv".into(),
                    build_statement: BuildStatement {
                        outputs: vec![
                            BuildOutput {
                                output_name: "bin".into(),
                                output_store_path: "/nix/store/hello-bin".into(),
                                nar_hash: nar_hash.into(),
                                nar_size: 1234,
                                references: vec!["/nix/store/glibc".into()],
                                closure_root: "/nix/store/hello-bin".into(),
                                content_addressed: Some("fixed:r:sha256:example".into()),
                            },
                            BuildOutput {
                                output_name: "man".into(),
                                output_store_path: "/nix/store/hello-man".into(),
                                nar_hash: format!("{nar_hash}-man"),
                                nar_size: 567,
                                references: vec![],
                                closure_root: "/nix/store/hello-man".into(),
                                content_addressed: None,
                            },
                        ],
                        build_log_digest: None,
                        sbom_digest: None,
                        test_result_digest: None,
                    },
                    built_at: finished_at,
                })),
            ],
        }
    }

    #[tokio::test]
    async fn normalized_store_round_trips_claims_in_order() {
        let store = EvidenceStore::in_memory().await.unwrap();
        let source = evidence("sha256-output");
        let receipt = store.insert(&source).await.unwrap();
        assert!(receipt.inserted);
        assert_eq!(receipt.evidence.evidence, source);

        let list = store.list("/nix/store/hello.drv").await.unwrap();
        assert_eq!(list.evidences.len(), 1);
        assert_eq!(list.evidences[0].evidence.claims, source.claims);
    }

    #[tokio::test]
    async fn normalized_store_keeps_duplicate_and_divergent_history() {
        let store = EvidenceStore::in_memory().await.unwrap();
        store.insert(&evidence("sha256-one")).await.unwrap();
        store.insert(&evidence("sha256-one")).await.unwrap();
        store.insert(&evidence("sha256-two")).await.unwrap();

        let list = store.list("/nix/store/hello.drv").await.unwrap();
        assert_eq!(list.evidences.len(), 3);
    }

    #[tokio::test]
    async fn separate_rounds_never_combine_into_cache_consensus() {
        let store = EvidenceStore::in_memory().await.unwrap();
        let config = RoundConfig {
            minimum_builders: 1,
            ..RoundConfig::default()
        };
        for builder_id in ["builder-a", "builder-b"] {
            let mut evidence = evidence("sha256-same");
            evidence.builder_id = builder_id.into();
            let nonce = shared::generate_nonce().unwrap();
            let commitment = CommitmentRequest {
                builder_id: builder_id.into(),
                derivation_path: "/nix/store/hello.drv".into(),
                digest: shared::evidence_commitment(&evidence, &nonce).unwrap(),
            };
            let receipt = store.commit(&commitment, config).await.unwrap();
            store
                .reveal(
                    &EvidenceReveal {
                        round_id: receipt.round.id,
                        nonce,
                        evidence,
                    },
                    config,
                )
                .await
                .unwrap();
        }

        assert!(
            store
                .output_consensus("/nix/store/hello-bin", 2, config)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn deadlines_expire_an_incomplete_round() {
        let store = EvidenceStore::in_memory().await.unwrap();
        let config = RoundConfig {
            minimum_builders: 2,
            commit_window: Duration::zero(),
            reveal_window: Duration::zero(),
        };
        let evidence = evidence("sha256-output");
        let nonce = shared::generate_nonce().unwrap();
        let receipt = store
            .commit(
                &CommitmentRequest {
                    builder_id: evidence.builder_id.clone(),
                    derivation_path: "/nix/store/hello.drv".into(),
                    digest: shared::evidence_commitment(&evidence, &nonce).unwrap(),
                },
                config,
            )
            .await
            .unwrap();

        let status = store.round_status(receipt.round.id, config).await.unwrap();
        assert_eq!(status.phase, RoundPhase::Expired);
        let error = store
            .reveal(
                &EvidenceReveal {
                    round_id: status.id,
                    nonce,
                    evidence,
                },
                config,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ProtocolError::Gone(_)));
    }

    #[tokio::test]
    async fn initialization_rejects_schema_before_multi_output_support() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database
            .execute_unprepared(
                "CREATE TABLE build_claims (claim_id INTEGER PRIMARY KEY, output_path TEXT NOT NULL)",
            )
            .await
            .unwrap();
        let store = EvidenceStore { database };

        let error = store.initialise().await.unwrap_err();
        assert!(error.to_string().contains("predates version 4"));
    }

    #[tokio::test]
    async fn failed_claim_insert_rolls_back_the_whole_evidence() {
        let store = EvidenceStore::in_memory().await.unwrap();
        store
            .database
            .execute_unprepared("DROP TABLE log_claims")
            .await
            .unwrap();

        assert!(store.insert(&evidence("sha256-output")).await.is_err());
        assert_eq!(
            evidence_entity::Entity::find()
                .count(&store.database)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            claim::Entity::find().count(&store.database).await.unwrap(),
            0
        );
    }
}
