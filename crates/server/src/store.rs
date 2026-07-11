use crate::entity::{build_claim, claim, evidence as evidence_entity, log_claim};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use sea_orm::{
    ActiveValue::NotSet, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
    DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Schema, Set, Statement,
    TransactionTrait, sea_query::Index,
};
use shared::{
    BuildClaim, Claim, Evidence, EvidenceList, EvidenceReceipt, LogClaim, Package, ResolvedSource,
    StoredEvidence,
};
use std::path::Path;

const BUILD_KIND: &str = "build";
const LOG_KIND: &str = "log";

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

        let schema = Schema::new(backend);
        for entity in [
            schema.create_table_from_entity(evidence_entity::Entity),
            schema.create_table_from_entity(claim::Entity),
            schema.create_table_from_entity(build_claim::Entity),
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

        Ok(())
    }

    pub async fn insert(&self, evidence: &Evidence) -> Result<EvidenceReceipt> {
        validate(evidence)?;
        let transaction = self.database.begin().await?;
        let evidence_result = evidence_entity::Entity::insert(evidence_entity::ActiveModel {
            id: NotSet,
            schema_version: Set(i64::from(evidence.schema_version)),
            builder_id: Set(evidence.builder_id.clone()),
            package_repository: Set(evidence.package.repository.clone()),
            package_name: Set(evidence.package.name.clone()),
            received_at: Set(Utc::now()),
        })
        .exec(&transaction)
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
            .exec(&transaction)
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
                        output_path: Set(build.output_path.clone()),
                        nar_hash: Set(build.nar_hash.clone()),
                        built_at: Set(build.built_at),
                    })
                    .exec(&transaction)
                    .await
                    .context("failed to store build claim")?;
                }
                Claim::Log(log) => {
                    log_claim::Entity::insert(log_claim::ActiveModel {
                        claim_id: Set(claim_result.last_insert_id),
                        stdout: Set(log.stdout.clone()),
                        stderr: Set(log.stderr.clone()),
                        started_at: Set(log.started_at),
                        finished_at: Set(log.finished_at),
                    })
                    .exec(&transaction)
                    .await
                    .context("failed to store log claim")?;
                }
            }
        }

        transaction.commit().await?;
        let stored = self
            .find_by_id(evidence_result.last_insert_id)
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
                    Claim::Build(BuildClaim {
                        source: ResolvedSource {
                            resolved_url: model.source_resolved_url,
                            revision: model.source_revision,
                            nar_hash: model.source_nar_hash,
                        },
                        derivation_path: model.derivation_path,
                        output_path: model.output_path,
                        nar_hash: model.nar_hash,
                        built_at: model.built_at,
                    })
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

pub fn validate(evidence: &Evidence) -> Result<()> {
    evidence.validate().map_err(anyhow::Error::msg)
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
                Claim::Build(BuildClaim {
                    source: ResolvedSource {
                        resolved_url: "flake:nixpkgs".into(),
                        revision: None,
                        nar_hash: Some("sha256-source".into()),
                    },
                    derivation_path: "/nix/store/hello.drv".into(),
                    output_path: "/nix/store/hello".into(),
                    nar_hash: nar_hash.into(),
                    built_at: finished_at,
                }),
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
