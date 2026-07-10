use anyhow::{Context, Result};
use reqwest::Client;
use shared::{BuildEvidence, EvidenceList, EvidenceReceipt};

pub struct RegistryClient {
    base_url: String,
    client: Client,
}

impl RegistryClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if base_url.is_empty() {
            anyhow::bail!("server URL must not be empty");
        }
        reqwest::Url::parse(&base_url).context("server URL is invalid")?;

        Ok(Self {
            base_url,
            client: Client::new(),
        })
    }

    pub async fn submit(&self, evidence: &BuildEvidence) -> Result<EvidenceReceipt> {
        self.client
            .post(self.endpoint("/v1/evidence")?)
            .json(evidence)
            .send()
            .await
            .context("failed to submit evidence to registry")?
            .error_for_status()
            .context("registry rejected evidence")?
            .json()
            .await
            .context("registry returned invalid evidence receipt")
    }

    pub async fn facts(&self, derivation_path: &str) -> Result<EvidenceList> {
        self.client
            .get(self.endpoint("/v1/evidence")?)
            .query(&[("derivation_path", derivation_path)])
            .send()
            .await
            .context("failed to fetch registry evidence")?
            .error_for_status()
            .context("registry rejected evidence lookup")?
            .json()
            .await
            .context("registry returned invalid evidence facts")
    }

    fn endpoint(&self, path: &str) -> Result<reqwest::Url> {
        reqwest::Url::parse(&format!("{}{path}", self.base_url))
            .context("failed to construct registry endpoint URL")
    }
}
