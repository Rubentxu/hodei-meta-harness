//! Availability gate (AC-4, pure): if OpenSandbox or the selected
//! isolation is NOT available, the answer is an INFORMED refusal — running
//! natively on the host (YOLO) is never a fallback. Builds happen inside
//! the sandbox or they do not happen.

use serde::{Deserialize, Serialize};

/// What the doctor observed about the environment required by a profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateInput {
    /// The pinned image is present locally (or the registry is reachable
    /// and the reference resolves).
    pub image_available: bool,
    /// The local image content matches the pinned digest. For an UNPINNED
    /// user image there is no digest to mismatch: the image is simply
    /// present (or not).
    pub digest_matches: bool,
    /// The restricted egress path required by the build (proxy for the
    /// docker backend) is reachable — measured FROM INSIDE a sandbox, not
    /// from the host.
    pub egress_available: bool,
    /// The OpenSandbox lifecycle API answers.
    pub opensandbox_available: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GateDecision {
    Proceed,
    /// Informed refusal with the exact missing pieces. NO fallback is
    /// implied: not running the build is the honest outcome.
    Refuse {
        missing: Vec<String>,
    },
}

pub fn decide(input: &GateInput) -> GateDecision {
    let mut missing = Vec::new();
    if !input.opensandbox_available {
        missing.push("opensandbox lifecycle api".to_string());
    }
    if !input.image_available {
        missing.push("pinned image (pull required; the daemon fetches it, the profile never runs without it)".to_string());
    } else if !input.digest_matches {
        missing.push("local image digest does not match the pinned digest".to_string());
    }
    if !input.egress_available {
        missing.push("restricted egress path (proxy)".to_string());
    }
    if missing.is_empty() {
        GateDecision::Proceed
    } else {
        GateDecision::Refuse { missing }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> GateInput {
        GateInput {
            image_available: true,
            digest_matches: true,
            egress_available: true,
            opensandbox_available: true,
        }
    }

    #[test]
    fn everything_available_proceeds() {
        assert_eq!(decide(&healthy()), GateDecision::Proceed);
    }

    #[test]
    fn missing_sandbox_refuses_and_never_yolo() {
        let mut input = healthy();
        input.opensandbox_available = false;
        match decide(&input) {
            GateDecision::Refuse { missing } => {
                assert!(missing.iter().any(|m| m.contains("opensandbox")));
            }
            GateDecision::Proceed => panic!("must refuse"),
        }
    }

    #[test]
    fn digest_mismatch_refuses_instead_of_trusting_a_foreign_image() {
        let mut input = healthy();
        input.digest_matches = false;
        match decide(&input) {
            GateDecision::Refuse { missing } => {
                assert!(missing.iter().any(|m| m.contains("digest")));
            }
            GateDecision::Proceed => panic!("must refuse"),
        }
    }

    #[test]
    fn no_egress_refuses_there_is_no_native_fallback() {
        let mut input = healthy();
        input.egress_available = false;
        assert!(matches!(decide(&input), GateDecision::Refuse { .. }));
    }
}
