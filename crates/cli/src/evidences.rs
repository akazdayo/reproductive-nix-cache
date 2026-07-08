mod logs;

pub trait EvidenceExt {
    fn claim(&self) -> &'static str;
}

impl EvidenceExt for shared::Evidences {
    fn claim(&self) -> &'static str {
        match self {
            shared::Evidences::Logs => "logs",
            shared::Evidences::IP => "IP",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EvidenceExt;

    #[test]
    fn logs_evidence_has_claim() {
        assert_eq!(shared::Evidences::Logs.claim(), "logs");
    }
}
