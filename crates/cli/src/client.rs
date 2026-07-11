use anyhow::{Context, Result};
use reqwest::Client;
use shared::{Evidence, EvidenceList, EvidenceReceipt};
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

    pub async fn submit(&self, evidence: &Evidence) -> Result<EvidenceReceipt> {
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
