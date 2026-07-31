use anyhow::{Context, Result};
use reqwest::Client;
use shared::{
    CommitmentReceipt, CommitmentRequest, EvidenceList, EvidenceReceipt, EvidenceReveal,
    RoundStatus,
};
use std::{
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

pub struct RegistryClient {
    base_url: reqwest::Url,
    client: Client,
}

impl RegistryClient {
    pub fn new(host: &Host) -> Result<Self> {
        let base_url = reqwest::Url::try_from(host).context("server URL is invalid")?;

        Ok(Self {
            base_url,
            client: Client::new(),
        })
    }

    pub async fn commit(&self, commitment: &CommitmentRequest) -> Result<CommitmentReceipt> {
        self.send_with_retry(
            || {
                Ok(self
                    .client
                    .post(self.endpoint("/v1/evidence/commitments")?)
                    .json(commitment))
            },
            "submit commitment to registry",
        )
        .await?
        .error_for_status()
        .context("registry rejected commitment")?
        .json()
        .await
        .context("registry returned invalid commitment receipt")
    }

    pub async fn round_status(&self, round_id: i64) -> Result<RoundStatus> {
        self.send_with_retry(
            || {
                Ok(self
                    .client
                    .get(self.endpoint(&format!("/v1/evidence/rounds/{round_id}"))?))
            },
            "fetch commit-reveal round",
        )
        .await?
        .error_for_status()
        .context("registry rejected round lookup")?
        .json()
        .await
        .context("registry returned invalid round status")
    }

    pub async fn reveal(&self, reveal: &EvidenceReveal) -> Result<EvidenceReceipt> {
        self.send_with_retry(
            || {
                Ok(self
                    .client
                    .post(self.endpoint("/v1/evidence/reveals")?)
                    .json(reveal))
            },
            "reveal evidence to registry",
        )
        .await?
        .error_for_status()
        .context("registry rejected evidence reveal")?
        .json()
        .await
        .context("registry returned invalid evidence receipt")
    }

    pub async fn round_facts(&self, round_id: i64) -> Result<EvidenceList> {
        self.send_with_retry(
            || {
                Ok(self
                    .client
                    .get(self.endpoint("/v1/evidence")?)
                    .query(&[("round_id", round_id)]))
            },
            "fetch registry evidence",
        )
        .await?
        .error_for_status()
        .context("registry rejected evidence lookup")?
        .json()
        .await
        .context("registry returned invalid evidence facts")
    }

    async fn send_with_retry<F>(
        &self,
        mut request: F,
        action: &'static str,
    ) -> Result<reqwest::Response>
    where
        F: FnMut() -> Result<reqwest::RequestBuilder>,
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
                Err(error) => {
                    return Err(error).with_context(|| format!("failed to {action}"));
                }
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(std::time::Duration::from_secs(2));
        }
    }

    fn endpoint(&self, path: &str) -> Result<reqwest::Url> {
        self.base_url
            .join(path.trim_start_matches('/'))
            .context("failed to construct registry endpoint URL")
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    #[test]
    fn parse_to_hosts() {
        let ipv4 = "127.0.0.1";
        let domain = "example.com";

        let parsed_ipv4: Host = ipv4.parse().unwrap();
        let parsed_domain: Host = domain.parse().unwrap();

        assert_eq!(parsed_ipv4, format!("{}:3000", ipv4).parse().unwrap());
        assert_eq!(
            parsed_ipv4,
            Host::Ip(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                3000,
            ))
        );
        assert_eq!(parsed_domain, Host::Domain(DomainName(domain.to_owned())));
    }

    #[test]
    fn domain_to_reqwest_url() {
        let domain: Host = "example.com".parse().unwrap();
        let url = reqwest::Url::try_from(&domain).unwrap();

        assert_eq!(url.as_str(), "https://example.com/");
    }

    #[test]
    fn ipv4_has_port() {
        assert!("127.0.0.1:3000".parse::<Host>().is_ok());
    }
}
