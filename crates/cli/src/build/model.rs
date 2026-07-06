use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct Output {
    pub package: Package,
    pub evidences: Evidences,
    pub nar_hash: String,
}

#[derive(Debug, Serialize)]
pub struct Package {
    pub name: String,
    pub repository: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Evidences {
    Logs,
}

/// JSON response from `nix path-info --json`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NixPathInfo {
    pub nar_hash: String,
}
