use anyhow::{Context, Result, bail};
use reqwest::Url;
use shared::CacheLocation;

pub fn normalize(location: &CacheLocation) -> Result<Url> {
    let mut url = Url::parse(&location.uri).context("invalid cache location URI")?;
    if url.scheme() != "http" && url.scheme() != "https" {
        bail!("cache location URI must use the http or https scheme");
    }
    if url.host_str().is_none() {
        bail!("cache location URI must include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("cache location URI must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("cache location URI must not contain a query or fragment");
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(uri: &str) -> CacheLocation {
        CacheLocation { uri: uri.into() }
    }

    #[test]
    fn accepts_http_and_https_and_normalizes_the_base_path() {
        assert_eq!(
            normalize(&location("https://cache.example.com/builds"))
                .unwrap()
                .as_str(),
            "https://cache.example.com/builds/"
        );
        assert!(normalize(&location("http://127.0.0.1:8080/")).is_ok());
    }

    #[test]
    fn rejects_unsupported_or_credentialed_locations() {
        assert!(normalize(&location("s3://cache/builds")).is_err());
        assert!(normalize(&location("https://user:secret@cache.example.com/")).is_err());
        assert!(normalize(&location("https://cache.example.com/?token=secret")).is_err());
        assert!(normalize(&location("https://cache.example.com/#fragment")).is_err());
    }
}
