mod logs;
use std::collections::HashSet;

struct CalcClaim {
    evidences: HashSet<shared::Evidences>,
    output: shared::Output,
}

impl CalcClaim {
    fn claim(&self) {
        for x in self.evidences.iter() {
            let output = match x {
                shared::Evidences::IP(None) => {}
                shared::Evidences::Logs(None) => {}
                _ => {}
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Claim;

    #[test]
    fn logs_evidence_has_claim() {
        assert_eq!(shared::Evidences::Logs.claim(), "logs");
    }
}
