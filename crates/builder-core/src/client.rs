use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;
use shared::{
    CommitmentReceipt, CommitmentRequest, EvidenceList, EvidenceReceipt, EvidenceReveal,
    RoundStatus,
};
use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DomainName(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Host {
    Domain(DomainName),
    Ip(SocketAddr),
}

impl FromStr for Host {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Ok(addr) = value.parse::<SocketAddr>() {
            return Ok(Self::Ip(addr));
        }
        if let Ok(ip) = value.parse::<IpAddr>() {
            return Ok(Self::Ip(SocketAddr::new(ip, 3000)));
        }

        let domain = value.trim_end_matches('.');
        if domain.is_empty()
            || domain.len() > 253
            || domain.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
        {
            return Err("invalid domain name".into());
        }
        Ok(Self::Domain(DomainName(domain.to_ascii_lowercase())))
    }
}

impl TryFrom<&Host> for reqwest::Url {
    type Error = <reqwest::Url as FromStr>::Err;

    fn try_from(host: &Host) -> Result<Self, Self::Error> {
        let url = match host {
            Host::Domain(DomainName(domain)) => format!("https://{domain}"),
            Host::Ip(addr) => format!("http://{addr}"),
        };
        reqwest::Url::parse(&url)
    }
}

#[derive(Debug)]
pub struct RegistryError {
    status: Option<StatusCode>,
    message: String,
}

impl RegistryError {
    pub fn status(&self) -> Option<StatusCode> {
        self.status
    }

    fn request(action: &str, error: impl fmt::Display) -> Self {
        Self {
            status: None,
            message: format!("failed to {action}: {error}"),
        }
    }

    async fn response(response: reqwest::Response, action: &str) -> Self {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let detail = body.chars().take(1024).collect::<String>();
        let message = if detail.trim().is_empty() {
            format!("registry rejected {action} with HTTP {status}")
        } else {
            format!("registry rejected {action} with HTTP {status}: {detail}")
        };
        Self {
            status: Some(status),
            message,
        }
    }
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RegistryError {}

pub struct RegistryClient {
    base_url: reqwest::Url,
    client: Client,
}

impl RegistryClient {
    pub fn new(host: &Host) -> Result<Self, RegistryError> {
        let base_url = reqwest::Url::try_from(host)
            .map_err(|error| RegistryError::request("construct registry URL", error))?;
        Ok(Self {
            base_url,
            client: Client::new(),
        })
    }

    pub async fn commit(
        &self,
        commitment: &CommitmentRequest,
    ) -> Result<CommitmentReceipt, RegistryError> {
        let response = self
            .send_with_retry(
                || {
                    Ok(self
                        .client
                        .post(self.endpoint("/v1/evidence/commitments")?)
                        .json(commitment))
                },
                "submit commitment",
            )
            .await?;
        decode(response, "commitment").await
    }

    pub async fn round_status(&self, round_id: i64) -> Result<RoundStatus, RegistryError> {
        let response = self
            .send_with_retry(
                || {
                    Ok(self
                        .client
                        .get(self.endpoint(&format!("/v1/evidence/rounds/{round_id}"))?))
                },
                "fetch commit-reveal round",
            )
            .await?;
        decode(response, "round lookup").await
    }

    pub async fn reveal(&self, reveal: &EvidenceReveal) -> Result<EvidenceReceipt, RegistryError> {
        let response = self
            .send_with_retry(
                || {
                    Ok(self
                        .client
                        .post(self.endpoint("/v1/evidence/reveals")?)
                        .json(reveal))
                },
                "reveal evidence",
            )
            .await?;
        decode(response, "evidence reveal").await
    }

    pub async fn round_facts(&self, round_id: i64) -> Result<EvidenceList, RegistryError> {
        let response = self
            .send_with_retry(
                || {
                    Ok(self
                        .client
                        .get(self.endpoint("/v1/evidence")?)
                        .query(&[("round_id", round_id)]))
                },
                "fetch registry evidence",
            )
            .await?;
        decode(response, "evidence lookup").await
    }

    async fn send_with_retry<F>(
        &self,
        mut request: F,
        action: &'static str,
    ) -> Result<reqwest::Response, RegistryError>
    where
        F: FnMut() -> Result<reqwest::RequestBuilder, RegistryError>,
    {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut delay = std::time::Duration::from_millis(250);
        loop {
            match request()?.send().await {
                Ok(response)
                    if !response.status().is_server_error()
                        || tokio::time::Instant::now() >= deadline =>
                {
                    return Ok(response);
                }
                Ok(_) => {}
                Err(error) if tokio::time::Instant::now() < deadline => {
                    let _ = error;
                }
                Err(error) => return Err(RegistryError::request(action, error)),
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(std::time::Duration::from_secs(2));
        }
    }

    fn endpoint(&self, path: &str) -> Result<reqwest::Url, RegistryError> {
        self.base_url
            .join(path.trim_start_matches('/'))
            .map_err(|error| RegistryError::request("construct registry endpoint URL", error))
    }
}

async fn decode<T: DeserializeOwned>(
    response: reqwest::Response,
    action: &str,
) -> Result<T, RegistryError> {
    if !response.status().is_success() {
        return Err(RegistryError::response(response, action).await);
    }
    response
        .json()
        .await
        .map_err(|error| RegistryError::request(&format!("decode registry {action}"), error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn host_parses_ip_and_domain() {
        let ipv4: Host = "127.0.0.1".parse().unwrap();
        let domain: Host = "example.com".parse().unwrap();
        assert_eq!(
            ipv4,
            Host::Ip(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                3000,
            ))
        );
        assert_eq!(
            reqwest::Url::try_from(&domain).unwrap().as_str(),
            "https://example.com/"
        );
    }
}
