use regex::Regex;
use shared::Package;
use std::sync::OnceLock;

pub fn parse_nix_repository(input: &str) -> Option<Package> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^(?P<repo>[^#\s]+)#(?P<package>[^#\s]+)$").unwrap());
    let caps = re.captures(input)?;

    Some(Package {
        repository: caps.name("repo")?.as_str().to_owned(),
        name: caps.name("package")?.as_str().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_package_reference() {
        assert_eq!(
            parse_nix_repository("nixpkgs#hello"),
            Some(Package {
                repository: "nixpkgs".into(),
                name: "hello".into(),
            })
        );
    }

    #[test]
    fn rejects_incomplete_or_whitespace_references() {
        for reference in ["nixpkgs", "#hello", "nixpkgs#", "nixpkgs#hello world"] {
            assert!(parse_nix_repository(reference).is_none(), "{reference}");
        }
    }
}
