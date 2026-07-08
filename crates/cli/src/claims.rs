mod logs;

pub trait EvidenceClaim {
    fn claim(&self) -> &'static str;
    fn into_claimed(self) -> Self;
}

pub struct CalcClaim {
    evidence: shared::Evidences,
    output: shared::Output,
}

impl CalcClaim {
    pub fn new(evidence: shared::Evidences, output: shared::Output) -> Self {
        Self { evidence, output }
    }

    pub fn into_output(mut self) -> shared::Output {
        self.output.evidences = self.evidence.into_claimed();
        self.output
    }
}

impl EvidenceClaim for shared::Evidences {
    fn claim(&self) -> &'static str {
        match self {
            shared::Evidences::Logs(_) => "logs",
            shared::Evidences::IP(_) => "ip",
        }
    }

    fn into_claimed(self) -> Self {
        match self {
            shared::Evidences::Logs(None) => {
                shared::Evidences::Logs(Some(shared::LogsClaim::new("logs")))
            }
            shared::Evidences::IP(None) => shared::Evidences::IP(Some(Default::default())),
            evidence => evidence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EvidenceClaim;

    #[test]
    fn logs_evidence_has_claim() {
        assert_eq!(shared::Evidences::Logs(None).claim(), "logs");
    }

    #[test]
    fn logs_evidence_is_claimed() {
        assert!(matches!(
            shared::Evidences::Logs(None).into_claimed(),
            shared::Evidences::Logs(Some(_))
        ));
    }
}
