use crate::store::{EvidenceStore, OutputFingerprint, RoundConfig};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use object_store::{
    GetResult, ObjectMeta, ObjectStore, ObjectStoreExt, aws::AmazonS3Builder,
    path::Path as ObjectPath,
};
use std::sync::Arc;
use url::Url;

pub const CACHE_INFO: &str = "StoreDir: /nix/store\nWantMassQuery: 0\nPriority: 30\n";

#[derive(Clone)]
pub struct BinaryCache {
    objects: Arc<dyn ObjectStore>,
    minimum_builders: usize,
}

impl BinaryCache {
    pub fn from_s3_url(store_url: &str, minimum_builders: usize) -> Result<Self> {
        let url = Url::parse(store_url).context("invalid binary cache URL")?;
        if url.scheme() != "s3" {
            bail!("binary cache URL must use the s3 scheme");
        }
        let bucket = url
            .host_str()
            .filter(|bucket| !bucket.is_empty())
            .context("binary cache URL must include a bucket name")?;

        let mut endpoint = None;
        let mut endpoint_scheme = "https".to_owned();
        let mut region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".into());
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "endpoint" => endpoint = Some(value.into_owned()),
                "scheme" => endpoint_scheme = value.into_owned(),
                "region" => region = value.into_owned(),
                _ => {}
            }
        }

        let mut builder = AmazonS3Builder::from_env()
            .with_bucket_name(bucket)
            .with_region(region);
        if let Some(endpoint) = endpoint {
            if endpoint_scheme != "http" && endpoint_scheme != "https" {
                bail!("binary cache endpoint scheme must be http or https");
            }
            builder = builder
                .with_endpoint(format!("{endpoint_scheme}://{endpoint}"))
                .with_allow_http(endpoint_scheme == "http");
        }
        let objects = builder
            .build()
            .context("failed to configure S3 binary cache")?;
        Ok(Self::new(Arc::new(objects), minimum_builders))
    }

    fn new(objects: Arc<dyn ObjectStore>, minimum_builders: usize) -> Self {
        Self {
            objects,
            minimum_builders,
        }
    }

    #[cfg(test)]
    pub fn for_tests(objects: Arc<dyn ObjectStore>, minimum_builders: usize) -> Self {
        Self::new(objects, minimum_builders)
    }

    pub async fn approved_narinfo(
        &self,
        evidence: &EvidenceStore,
        key: &str,
        round_config: RoundConfig,
    ) -> Result<Option<Vec<u8>>> {
        let Some(store_hash) = key.strip_suffix(".narinfo").filter(|hash| valid_hash(hash)) else {
            return Ok(None);
        };
        let Some(result) = self.get(key).await? else {
            return Ok(None);
        };
        let bytes = result
            .bytes()
            .await
            .context("failed to read narinfo from object storage")?;
        let narinfo = NarInfo::parse(&bytes)?;
        if narinfo.store_hash() != Some(store_hash) {
            return Ok(None);
        }
        let Some(consensus) = evidence
            .output_consensus(&narinfo.store_path, self.minimum_builders, round_config)
            .await?
        else {
            return Ok(None);
        };
        if !narinfo.matches(&consensus) {
            return Ok(None);
        }

        Ok(Some(bytes.to_vec()))
    }

    pub async fn get_nar(&self, key: &str) -> Result<Option<GetResult>> {
        let Some(key) = nar_key(key) else {
            return Ok(None);
        };
        self.get(&key).await
    }

    pub async fn head_nar(&self, key: &str) -> Result<Option<ObjectMeta>> {
        let Some(key) = nar_key(key) else {
            return Ok(None);
        };
        let path = ObjectPath::parse(key).context("invalid NAR object key")?;
        match self.objects.head(&path).await {
            Ok(meta) => Ok(Some(meta)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(error).context("failed to read NAR metadata from object storage"),
        }
    }

    async fn get(&self, key: &str) -> Result<Option<GetResult>> {
        let path = ObjectPath::parse(key).context("invalid binary cache object key")?;
        match self.objects.get(&path).await {
            Ok(result) => Ok(Some(result)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(error).context("failed to read from binary cache object storage"),
        }
    }
}

struct NarInfo {
    store_path: String,
    nar_hash: String,
    nar_size: u64,
    references: Vec<String>,
}

impl NarInfo {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let contents = std::str::from_utf8(bytes).context("narinfo is not UTF-8")?;
        let required = |name: &str| {
            contents
                .lines()
                .find_map(|line| {
                    line.strip_prefix(name)
                        .and_then(|line| line.strip_prefix(": "))
                })
                .map(str::to_owned)
                .with_context(|| format!("narinfo is missing {name}"))
        };
        let store_path = required("StorePath")?;
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

fn nar_key(key: &str) -> Option<String> {
    if key.is_empty() || key.contains("..") || key.starts_with('/') {
        return None;
    }
    Some(format!("nar/{key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_matches_narinfo() {
        let bytes = b"StorePath: /nix/store/00000000000000000000000000000000-hello\n\
URL: nar/example.nar.xz\n\
Compression: xz\n\
NarHash: sha256:0f3gg73cybjfnzlav06r5ndr4711wv2gjkgk2s0lghp2h3cy6db7\n\
NarSize: 1234\n\
References: b-glibc a-libgcc\n";
        let narinfo = NarInfo::parse(bytes).unwrap();
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
    fn rejects_unsafe_nar_keys() {
        assert_eq!(nar_key("example.nar.xz"), Some("nar/example.nar.xz".into()));
        assert_eq!(
            nar_key("example.nar.zst"),
            Some("nar/example.nar.zst".into())
        );
        assert_eq!(nar_key("../example.nar.xz"), None);
        assert_eq!(nar_key("/example.nar.xz"), None);
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
