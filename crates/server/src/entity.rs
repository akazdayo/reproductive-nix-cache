pub mod evidence {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "evidences")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub schema_version: i64,
        pub builder_id: String,
        pub package_repository: String,
        pub package_name: String,
        pub received_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(has_many = "super::claim::Entity")]
        Claim,
    }

    impl Related<super::claim::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Claim.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod claim {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "claims")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub evidence_id: i64,
        pub position: i64,
        pub kind: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::evidence::Entity",
            from = "Column::EvidenceId",
            to = "super::evidence::Column::Id",
            on_update = "Cascade",
            on_delete = "Cascade"
        )]
        Evidence,
        #[sea_orm(has_one = "super::build_claim::Entity")]
        BuildClaim,
        #[sea_orm(has_one = "super::log_claim::Entity")]
        LogClaim,
    }

    impl Related<super::evidence::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Evidence.def()
        }
    }

    impl Related<super::build_claim::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::BuildClaim.def()
        }
    }

    impl Related<super::log_claim::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::LogClaim.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod build_claim {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "build_claims")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub claim_id: i64,
        pub source_resolved_url: String,
        pub source_revision: Option<String>,
        pub source_nar_hash: Option<String>,
        pub derivation_path: String,
        pub build_log_digest: Option<String>,
        pub sbom_digest: Option<String>,
        pub test_result_digest: Option<String>,
        pub built_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::claim::Entity",
            from = "Column::ClaimId",
            to = "super::claim::Column::Id",
            on_update = "Cascade",
            on_delete = "Cascade"
        )]
        Claim,
        #[sea_orm(has_many = "super::build_output::Entity")]
        BuildOutput,
    }

    impl Related<super::claim::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Claim.def()
        }
    }

    impl Related<super::build_output::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::BuildOutput.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod build_output {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "build_outputs")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub claim_id: i64,
        #[sea_orm(primary_key, auto_increment = false)]
        pub position: i64,
        pub output_name: String,
        pub output_store_path: String,
        pub nar_hash: String,
        pub nar_size: i64,
        pub references_json: String,
        pub closure_root: String,
        pub content_addressed: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::build_claim::Entity",
            from = "Column::ClaimId",
            to = "super::build_claim::Column::ClaimId",
            on_update = "Cascade",
            on_delete = "Cascade"
        )]
        BuildClaim,
    }

    impl Related<super::build_claim::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::BuildClaim.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod log_claim {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "log_claims")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub claim_id: i64,
        pub stdout: String,
        pub stderr: String,
        pub started_at: DateTimeUtc,
        pub finished_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::claim::Entity",
            from = "Column::ClaimId",
            to = "super::claim::Column::Id",
            on_update = "Cascade",
            on_delete = "Cascade"
        )]
        Claim,
    }

    impl Related<super::claim::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Claim.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}
