use crate::store::{ApprovedOutput, CacheSource, EvidenceStore, OutputFingerprint, RoundConfig};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, Response, StatusCode, Url, redirect::Policy};
use std::time::Duration;

pub const CACHE_INFO: &str = "StoreDir: /nix/store\nWantMassQuery: 0\nPriority: 30\n";
const MAX_NARINFO_SIZE: usize = 1024 * 1024;

#[derive(Clone)]
pub struct BinaryCache {
    client: Client,
    minimum_builders: usize,
}

pub struct ApprovedNarInfo {
    pub bytes: Vec<u8>,
    upstream_nar_url: Url,
}

impl BinaryCache {
    pub fn new(minimum_builders: usize) -> Result<Self> {
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .build()
            .context("failed to configure HTTP binary cache client")?;
        Ok(Self {
            client,
            minimum_builders,
        })
    }

    #[cfg(test)]
    pub fn for_tests(minimum_builders: usize) -> Self {
        Self {
            client: Client::builder()
                .redirect(Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
            minimum_builders,
        }
    }

    pub async fn approved_narinfo(
        &self,
        evidence: &EvidenceStore,
        key: &str,
        round_config: RoundConfig,
    ) -> Result<Option<ApprovedNarInfo>> {
        let Some(store_hash) = key.strip_suffix(".narinfo").filter(|hash| valid_hash(hash)) else {
            return Ok(None);
        };
        let Some(approved) = evidence
            .approved_output(store_hash, self.minimum_builders, round_config)
            .await?
        else {
            return Ok(None);
        };
        let mut last_error = None;
        for source in &approved.sources {
            match self
                .approved_from_source(&approved, source, store_hash, key)
                .await
            {
                Ok(Some(narinfo)) => return Ok(Some(narinfo)),
                Ok(None) => {}
                Err(error) => {
                    last_error = Some(error.context(format!(
                        "cache location {} could not serve approved narinfo",
                        source.id
                    )));
                }
            }
        }
        match last_error {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    pub async fn get_nar(
        &self,
        evidence: &EvidenceStore,
        store_hash: &str,
        location_id: i64,
        round_config: RoundConfig,
    ) -> Result<Option<Response>> {
        let Some(approved) = self
            .approved_by_hash(evidence, store_hash, location_id, round_config)
            .await?
        else {
            return Ok(None);
        };
        self.send_nar(self.client.get(approved.upstream_nar_url))
            .await
    }

    pub async fn head_nar(
        &self,
        evidence: &EvidenceStore,
        store_hash: &str,
        location_id: i64,
        round_config: RoundConfig,
    ) -> Result<Option<Response>> {
        let Some(approved) = self
            .approved_by_hash(evidence, store_hash, location_id, round_config)
            .await?
        else {
            return Ok(None);
        };
        self.send_nar(self.client.head(approved.upstream_nar_url))
            .await
    }

    async fn approved_by_hash(
        &self,
        evidence: &EvidenceStore,
        store_hash: &str,
        location_id: i64,
        round_config: RoundConfig,
    ) -> Result<Option<ApprovedNarInfo>> {
        if !valid_hash(store_hash) {
            return Ok(None);
        }
        let Some(approved) = evidence
            .approved_output(store_hash, self.minimum_builders, round_config)
            .await?
        else {
            return Ok(None);
        };
        let Some(source) = approved
            .sources
            .iter()
            .find(|source| source.id == location_id)
        else {
            return Ok(None);
        };
        self.approved_from_source(
            &approved,
            source,
            store_hash,
            &format!("{store_hash}.narinfo"),
        )
        .await
    }

    async fn approved_from_source(
        &self,
        approved: &ApprovedOutput,
        source: &CacheSource,
        store_hash: &str,
        key: &str,
    ) -> Result<Option<ApprovedNarInfo>> {
        let upstream = Url::parse(&source.uri).context("stored cache location URI is invalid")?;
        let Some(bytes) = self.get_narinfo(&upstream, key).await? else {
            return Ok(None);
        };
        let narinfo = NarInfo::parse(&bytes)?;
        if narinfo.store_hash() != Some(store_hash)
            || narinfo.store_path != approved.store_path
            || !narinfo.matches(&approved.fingerprint)
        {
            return Ok(None);
        }
        let upstream_nar_url = Self::resolve_nar_url(&upstream, &narinfo.url)?;
        let bytes = rewrite_nar_url(&bytes, &format!("nar/{store_hash}/{}", source.id))?;
        Ok(Some(ApprovedNarInfo {
            bytes,
            upstream_nar_url,
        }))
    }

    async fn get_narinfo(&self, upstream: &Url, key: &str) -> Result<Option<Vec<u8>>> {
        let url = upstream
            .join(key)
            .context("failed to construct upstream narinfo URL")?;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("failed to request narinfo from upstream cache")?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!("upstream cache returned {} for narinfo", response.status());
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_NARINFO_SIZE as u64)
        {
            bail!("upstream narinfo exceeds {MAX_NARINFO_SIZE} bytes");
        }
        let bytes = response
            .bytes()
            .await
            .context("failed to read narinfo from upstream cache")?;
        if bytes.len() > MAX_NARINFO_SIZE {
            bail!("upstream narinfo exceeds {MAX_NARINFO_SIZE} bytes");
        }
        Ok(Some(bytes.to_vec()))
    }

    async fn send_nar(&self, request: reqwest::RequestBuilder) -> Result<Option<Response>> {
        let response = request
            .send()
            .await
            .context("failed to request NAR from upstream cache")?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!("upstream cache returned {} for NAR", response.status());
        }
        Ok(Some(response))
    }

    fn resolve_nar_url(upstream: &Url, value: &str) -> Result<Url> {
        let resolved = upstream
            .join(value)
            .context("narinfo contains an invalid URL")?;
        let same_origin = resolved.scheme() == upstream.scheme()
            && resolved.host_str() == upstream.host_str()
            && resolved.port_or_known_default() == upstream.port_or_known_default();
        if !same_origin {
            bail!("narinfo URL escapes the cache location origin");
        }
        Ok(resolved)
    }
}

struct NarInfo {
    store_path: String,
    url: String,
    nar_hash: String,
    nar_size: u64,
    references: Vec<String>,
}

impl NarInfo {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let contents = std::str::from_utf8(bytes).context("narinfo is not UTF-8")?;
        if !contents.ends_with('\n') {
            bail!("narinfo does not end with a newline");
        }
        let required = |name: &str| {
            let prefix = format!("{name}: ");
            let mut values = contents
                .lines()
                .filter_map(|line| line.strip_prefix(&prefix));
            let value = values
                .next()
                .with_context(|| format!("narinfo is missing {name}"))?;
            if values.next().is_some() {
                bail!("narinfo contains duplicate {name}");
            }
            Ok(value.to_owned())
        };
        let store_path = required("StorePath")?;
        let url = required("URL")?;
        let nar_hash = required("NarHash")?;
        let nar_size = required("NarSize")?
            .parse()
            .context("narinfo NarSize is invalid")?;
        let references = contents
            .lines()
            .find_map(|line| line.strip_prefix("References:"))
            .map(|references| {
                references
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(Self {
            store_path,
            url,
            nar_hash,
            nar_size,
            references,
        })
    }

    fn store_hash(&self) -> Option<&str> {
        self.store_path
            .strip_prefix("/nix/store/")?
            .split_once('-')
            .map(|(hash, _)| hash)
    }

    fn matches(&self, fingerprint: &OutputFingerprint) -> bool {
        let mut references = self.references.clone();
        references.sort();
        references.dedup();
        hashes_equal(&self.nar_hash, &fingerprint.nar_hash)
            && self.nar_size == fingerprint.nar_size
            && references == fingerprint.references
    }
}

fn rewrite_nar_url(bytes: &[u8], gateway_url: &str) -> Result<Vec<u8>> {
    let contents = std::str::from_utf8(bytes).context("narinfo is not UTF-8")?;
    let mut rewritten = String::with_capacity(contents.len());
    let mut found = false;
    for line in contents.split_inclusive('\n') {
        if line
            .strip_suffix('\n')
            .is_some_and(|line| line.starts_with("URL: "))
        {
            if found {
                bail!("narinfo contains duplicate URL");
            }
            found = true;
            rewritten.push_str("URL: ");
            rewritten.push_str(gateway_url);
            rewritten.push('\n');
        } else {
            rewritten.push_str(line);
        }
    }
    if !found {
        bail!("narinfo is missing URL");
    }
    Ok(rewritten.into_bytes())
}

fn hashes_equal(left: &str, right: &str) -> bool {
    parse_sha256(left)
        .zip(parse_sha256(right))
        .is_some_and(|(left, right)| left == right)
}

fn parse_sha256(value: &str) -> Option<Vec<u8>> {
    let bytes = if let Some(encoded) = value.strip_prefix("sha256-") {
        STANDARD.decode(encoded).ok()?
    } else if let Some(encoded) = value.strip_prefix("sha256:") {
        nix_base32::from_nix_base32(encoded)?
    } else {
        return None;
    };
    (bytes.len() == 32).then_some(bytes)
}

fn valid_hash(hash: &str) -> bool {
    const NIX_BASE32: &str = "0123456789abcdfghijklmnpqrsvwxyz";
    hash.len() == 32 && hash.chars().all(|character| NIX_BASE32.contains(character))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NARINFO: &[u8] = b"StorePath: /nix/store/00000000000000000000000000000000-hello\n\
URL: nar/example.nar.xz\n\
Compression: xz\n\
NarHash: sha256:0f3gg73cybjfnzlav06r5ndr4711wv2gjkgk2s0lghp2h3cy6db7\n\
NarSize: 1234\n\
References: b-glibc a-libgcc\n";

    #[test]
    fn parses_and_matches_narinfo() {
        let narinfo = NarInfo::parse(NARINFO).unwrap();
        assert_eq!(
            narinfo.store_hash(),
            Some("00000000000000000000000000000000")
        );
        assert!(narinfo.matches(&OutputFingerprint {
            nar_hash: "sha256-ZzXj2YDiwkeBFvNN+cTmIRySmy3ZgK3ot04uz8Z5bzg=".into(),
            nar_size: 1234,
            references: vec!["a-libgcc".into(), "b-glibc".into()],
        }));
    }

    #[test]
    fn rewrites_only_the_nar_url() {
        let rewritten = rewrite_nar_url(NARINFO, "nar/00000000000000000000000000000000").unwrap();
        let rewritten = String::from_utf8(rewritten).unwrap();
        assert!(rewritten.contains("URL: nar/00000000000000000000000000000000\n"));
        assert!(rewritten.contains("NarSize: 1234\n"));
    }

    #[test]
    fn rejects_nar_urls_outside_the_cache_origin() {
        let upstream = Url::parse("https://cache.example.com/private/").unwrap();
        assert!(BinaryCache::resolve_nar_url(&upstream, "nar/example.nar.xz").is_ok());
        assert!(BinaryCache::resolve_nar_url(&upstream, "/nar/example.nar.xz").is_ok());
        assert!(
            BinaryCache::resolve_nar_url(&upstream, "https://evil.example/nar/example.nar.xz")
                .is_err()
        );
    }

    #[test]
    fn compares_sri_and_nix_base32_hashes() {
        assert!(hashes_equal(
            "sha256-ZzXj2YDiwkeBFvNN+cTmIRySmy3ZgK3ot04uz8Z5bzg=",
            "sha256:0f3gg73cybjfnzlav06r5ndr4711wv2gjkgk2s0lghp2h3cy6db7"
        ));
        assert!(!hashes_equal(
            "sha256-ZzXj2YDiwkeBFvNN+cTmIRySmy3ZgK3ot04uz8Z5bzg=",
            "sha256:0000000000000000000000000000000000000000000000000000"
        ));
    }
}
