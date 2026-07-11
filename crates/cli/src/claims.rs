use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ClaimKind {
    Build,
    Log,
}

impl ClaimKind {
    pub fn with_required_build(requested: Vec<Self>) -> Vec<Self> {
        let mut enabled = vec![Self::Build];
        for kind in requested {
            if !enabled.contains(&kind) {
                enabled.push(kind);
            }
        }
        enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_build_is_first_and_requested_claims_are_deduplicated() {
        assert_eq!(
            ClaimKind::with_required_build(vec![ClaimKind::Log, ClaimKind::Build, ClaimKind::Log,]),
            vec![ClaimKind::Build, ClaimKind::Log]
        );
    }
}
