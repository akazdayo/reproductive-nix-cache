use serde::Deserialize;

/// JSON response from `nix path-info --json`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NixPathInfo {
    pub nar_hash: String,
}
