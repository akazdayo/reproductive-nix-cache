use crate::entity;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use sea_orm::{
    ActiveValue::NotSet,
    ColumnTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, EntityTrait,
    QueryFilter, QueryOrder, Schema, Set, TryInsertResult,
    sea_query::{Index, OnConflict},
};
use shared::{
    BuildEvidence, EvidenceList, EvidenceReceipt, Package, ResolvedSource, StoredEvidence,
};
use std::path::Path;

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
        let schema = Schema::new(backend);
        let mut table = schema.create_table_from_entity(entity::Entity);
        table.if_not_exists();
        self.database
            .execute(backend.build(&table))
            .await
            .context("failed to create evidence table through SeaORM")?;

        let mut identity_index = Index::create();
        identity_index
            .name("evidence_builder_derivation_nar_uidx")
            .table(entity::Entity)
            .col(entity::Column::BuilderId)
            .col(entity::Column::DerivationPath)
            .col(entity::Column::NarHash)
            .unique()
            .if_not_exists();
        self.database
            .execute(backend.build(&identity_index))
            .await
            .context("failed to create evidence identity index through SeaORM")?;

        let mut derivation_index = Index::create();
        derivation_index
            .name("evidence_derivation_path_idx")
            .table(entity::Entity)
            .col(entity::Column::DerivationPath)
            .col(entity::Column::Id)
            .if_not_exists();
        self.database
            .execute(backend.build(&derivation_index))
            .await
            .context("failed to create derivation lookup index through SeaORM")?;

        Ok(())
    }

    pub async fn insert(&self, evidence: &BuildEvidence) -> Result<EvidenceReceipt> {
        let active_model = entity::ActiveModel {
            id: NotSet,
            schema_version: Set(i64::from(evidence.schema_version)),
            builder_id: Set(evidence.builder_id.clone()),
            package_repository: Set(evidence.package.repository.clone()),
            package_name: Set(evidence.package.name.clone()),
            source_resolved_url: Set(evidence.source.resolved_url.clone()),
            source_revision: Set(evidence.source.revision.clone()),
            source_nar_hash: Set(evidence.source.nar_hash.clone()),
            derivation_path: Set(evidence.derivation_path.clone()),
            output_path: Set(evidence.output_path.clone()),
            nar_hash: Set(evidence.nar_hash.clone()),
            built_at: Set(evidence.built_at),
            received_at: Set(Utc::now()),
        };
        let conflict = OnConflict::columns([
            entity::Column::BuilderId,
            entity::Column::DerivationPath,
            entity::Column::NarHash,
        ])
        .do_nothing()
        .to_owned();
        let result = entity::Entity::insert(active_model)
            .on_conflict(conflict)
            .do_nothing()
            .exec(&self.database)
            .await
            .context("failed to store evidence through SeaORM")?;

        let (inserted, model) = match result {
            TryInsertResult::Inserted(result) => (
                true,
                self.find_by_id(result.last_insert_id)
                    .await?
                    .context("inserted evidence was not found")?,
            ),
            TryInsertResult::Conflicted => (
                false,
                self.find_by_identity(
                    &evidence.builder_id,
                    &evidence.derivation_path,
                    &evidence.nar_hash,
                )
                .await?
                .context("evidence disappeared after duplicate detection")?,
            ),
            TryInsertResult::Empty => bail!("SeaORM produced an empty evidence insert"),
        };

        Ok(EvidenceReceipt {
            inserted,
            evidence: model.try_into()?,
        })
    }

    pub async fn list(&self, derivation_path: &str) -> Result<EvidenceList> {
        let models = entity::Entity::find()
            .filter(entity::Column::DerivationPath.eq(derivation_path))
            .order_by_asc(entity::Column::Id)
            .all(&self.database)
            .await
            .context("failed to query evidence through SeaORM")?;
        let evidences = models
            .into_iter()
            .map(StoredEvidence::try_from)
            .collect::<Result<Vec<_>>>()?;

        Ok(EvidenceList {
            derivation_path: derivation_path.to_owned(),
            evidences,
        })
    }

    async fn find_by_id(&self, id: i64) -> Result<Option<entity::Model>> {
        entity::Entity::find_by_id(id)
            .one(&self.database)
            .await
            .context("failed to read inserted evidence through SeaORM")
    }

    async fn find_by_identity(
        &self,
        builder_id: &str,
        derivation_path: &str,
        nar_hash: &str,
    ) -> Result<Option<entity::Model>> {
        entity::Entity::find()
            .filter(entity::Column::BuilderId.eq(builder_id))
            .filter(entity::Column::DerivationPath.eq(derivation_path))
            .filter(entity::Column::NarHash.eq(nar_hash))
            .one(&self.database)
            .await
            .context("failed to read existing evidence through SeaORM")
    }
}

impl TryFrom<entity::Model> for StoredEvidence {
    type Error = anyhow::Error;

    fn try_from(model: entity::Model) -> Result<Self> {
        Ok(Self {
            id: model.id,
            evidence: BuildEvidence {
                schema_version: u32::try_from(model.schema_version)
                    .context("stored schema version is outside the u32 range")?,
                builder_id: model.builder_id,
                package: Package {
                    repository: model.package_repository,
                    name: model.package_name,
                },
                source: ResolvedSource {
                    resolved_url: model.source_resolved_url,
                    revision: model.source_revision,
                    nar_hash: model.source_nar_hash,
                },
                derivation_path: model.derivation_path,
                output_path: model.output_path,
                nar_hash: model.nar_hash,
                built_at: model.built_at,
            },
            received_at: model.received_at,
        })
    }
}

pub fn validate(evidence: &BuildEvidence) -> Result<()> {
    if evidence.schema_version != shared::EVIDENCE_SCHEMA_VERSION {
        bail!(
            "unsupported evidence schema version {}; expected {}",
            evidence.schema_version,
            shared::EVIDENCE_SCHEMA_VERSION
        );
    }

    for (name, value, max_len) in [
        ("builder_id", evidence.builder_id.as_str(), 128),
        (
            "package.repository",
            evidence.package.repository.as_str(),
            2048,
        ),
        ("package.name", evidence.package.name.as_str(), 1024),
        (
            "source.resolved_url",
            evidence.source.resolved_url.as_str(),
            4096,
        ),
        ("derivation_path", evidence.derivation_path.as_str(), 4096),
        ("output_path", evidence.output_path.as_str(), 4096),
        ("nar_hash", evidence.nar_hash.as_str(), 1024),
    ] {
        if value.trim().is_empty() {
            bail!("{name} must not be empty");
        }
        if value.len() > max_len {
            bail!("{name} exceeds the maximum length of {max_len}");
        }
    }

    Ok(())
}
