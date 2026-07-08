use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::IpAddr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    pub package: Package,
    pub evidences: Evidences,
    pub nar_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub repository: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct LogsClaim {
    pub log: String,
    pub timestamp: DateTime<Utc>,
}

impl LogsClaim {
    pub fn new(log: impl Into<String>) -> Self {
        Self {
            log: log.into(),
            timestamp: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Evidences {
    Logs(Option<LogsClaim>),
    IP(Option<HashSet<IpAddr>>),
}
