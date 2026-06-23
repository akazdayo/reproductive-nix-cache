use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Output {
    pub package: Package,
    pub evidences: Evidences,
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
