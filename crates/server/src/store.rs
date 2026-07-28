use crate::entity::{
    build_claim, build_output as build_output_entity, claim, evidence as evidence_entity, log_claim,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use sea_orm::{
    ActiveValue::NotSet, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
    DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Schema, Set, Statement,
    TransactionTrait, sea_query::Index,
};
use shared::{
    BuildClaim, BuildOutput, BuildStatement, Claim, Evidence, EvidenceList, EvidenceReceipt,
    LogClaim, Package, ResolvedSource, StoredEvidence,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const BUILD_KIND: &str = "build";
const LOG_KIND: &str = "log";

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
            schema.create_table_from_entity(evidence_entity::Entity),
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
                        build_log_digest: Set(build.build_statement.build_log_digest.clone()),
                        sbom_digest: Set(build.build_statement.sbom_digest.clone()),
                        test_result_digest: Set(build.build_statement.test_result_digest.clone()),
                        built_at: Set(build.built_at),
                    })
                    .exec(&transaction)
                    .await
                    .context("failed to store build claim")?;

                    for (position, output) in build.build_statement.outputs.iter().enumerate() {
                        build_output_entity::Entity::insert(build_output_entity::ActiveModel {
                            claim_id: Set(claim_result.last_insert_id),
                            position: Set(
                                i64::try_from(position).context("too many build outputs")?
                            ),
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
                        .exec(&transaction)
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

    pub async fn output_consensus(
        &self,
        store_path: &str,
        minimum_builders: usize,
    ) -> Result<Option<OutputFingerprint>> {
        let output_rows = build_output_entity::Entity::find()
            .filter(build_output_entity::Column::OutputStorePath.eq(store_path))
            .all(&self.database)
            .await
            .context("failed to query build output evidence")?;
        let mut variants: BTreeMap<OutputFingerprint, BTreeSet<String>> = BTreeMap::new();
        for output in output_rows {
            let claim = claim::Entity::find_by_id(output.claim_id)
                .one(&self.database)
                .await?
                .context("build output claim envelope is missing")?;
            let evidence = evidence_entity::Entity::find_by_id(claim.evidence_id)
                .one(&self.database)
                .await?
                .context("build output evidence envelope is missing")?;
            variants
                .entry(OutputFingerprint::from_model(&output)?)
                .or_default()
                .insert(evidence.builder_id);
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
