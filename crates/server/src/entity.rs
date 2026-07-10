use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "evidence")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub schema_version: i64,
    pub builder_id: String,
    pub package_repository: String,
    pub package_name: String,
    pub source_resolved_url: String,
    pub source_revision: Option<String>,
    pub source_nar_hash: Option<String>,
    pub derivation_path: String,
    pub output_path: String,
    pub nar_hash: String,
    pub built_at: DateTimeUtc,
    pub received_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
