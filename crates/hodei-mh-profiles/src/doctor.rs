//! Doctor (pure plan + host adapter): NON-destructive checks for a clean
//! host — what must be true before a profile runs. The doctor NEVER
//! creates, starts or removes anything; every check is a read.

use serde::{Deserialize, Serialize};

use crate::gate::GateInput;
use crate::profile::ProfileSpec;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    /// Read-only observation, safe to show (no secrets).
    pub detail: String,
}

/// The doctor's plan for a profile. Executing each check is the adapter's
/// job; the DECISION (gate) stays pure.
pub fn checks_for(
    spec: &ProfileSpec,
    image_local: Option<&str>,
    egress_reachable: bool,
    opensandbox_ready: bool,
    subscription: &crate::credentials::SubscriptionNote,
) -> (Vec<DoctorCheck>, GateInput) {
    let (image_ok, image_check) = match image_local {
        Some(local_digest) => {
            let matches = spec.image.digest.as_deref() == Some(local_digest);
            let check = DoctorCheck {
                name: "pinned-image-local".into(),
                ok: matches,
                detail: match (&spec.image.digest, local_digest) {
                    (Some(expected), local) => format!(
                        "pinned {expected}; local {local}{}",
                        if matches { "" } else { " (MISMATCH)" }
                    ),
                    (None, _) => "user image without a pin".into(),
                },
            };
            (matches, check)
        }
        None => (
            false,
            DoctorCheck {
                name: "pinned-image-local".into(),
                ok: false,
                detail: format!(
                    "{} not present locally — the docker daemon must pull it first (the profile never runs without it)",
                    spec.image.full_reference()
                ),
            },
        ),
    };
    let checks = vec![
        DoctorCheck {
            name: "profile-pinned".into(),
            ok: spec.image.digest.is_some(),
            detail: spec.image.full_reference(),
        },
        image_check,
        DoctorCheck {
            name: "restricted-egress".into(),
            ok: egress_reachable,
            detail: if egress_reachable {
                "egress proxy reachable (docker backend isolation holds)".into()
            } else {
                "egress proxy NOT reachable — builds needing the network would refuse to run".into()
            },
        },
        DoctorCheck {
            name: "opensandbox-api".into(),
            ok: opensandbox_ready,
            detail: if opensandbox_ready {
                "lifecycle API answers".into()
            } else {
                "lifecycle API unreachable — no sandbox can be created".into()
            },
        },
        DoctorCheck {
            name: "subscription-limitations".into(),
            ok: true,
            detail: format!("tier {}: {}", subscription.tier, subscription.limitation),
        },
    ];
    let input = GateInput {
        image_available: image_local.is_some(),
        digest_matches: image_ok,
        egress_available: egress_reachable,
        opensandbox_available: opensandbox_ready,
    };
    (checks, input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{ImagePin, rust_profile};

    #[test]
    fn doctor_reports_every_dimension_and_never_claims_all_ok_when_something_is_missing() {
        let spec = rust_profile(ImagePin::pinned("rust:1-bookworm", "sha256:correct"));
        let subscription = crate::credentials::SubscriptionNote {
            tier: "none".into(),
            limitation: "no credentials configured".into(),
        };
        let (checks, input) = checks_for(&spec, Some("sha256:WRONG"), false, true, &subscription);
        assert_eq!(checks.len(), 5);
        let image_check = checks
            .iter()
            .find(|c| c.name == "pinned-image-local")
            .expect("check");
        assert!(!image_check.ok);
        assert!(image_check.detail.contains("MISMATCH"));
        let egress = checks
            .iter()
            .find(|c| c.name == "restricted-egress")
            .expect("check");
        assert!(!egress.ok);
        assert!(matches!(
            crate::gate::decide(&input),
            crate::gate::GateDecision::Refuse { .. }
        ));
    }

    #[test]
    fn missing_image_is_reported_without_pulling() {
        let spec = rust_profile(ImagePin::pinned("rust:1-bookworm", "sha256:r"));
        let subscription = crate::credentials::SubscriptionNote {
            tier: "anthropic-api".into(),
            limitation: "standard tier".into(),
        };
        let (checks, _) = checks_for(&spec, None, true, true, &subscription);
        let image_check = checks
            .iter()
            .find(|c| c.name == "pinned-image-local")
            .expect("check");
        assert!(!image_check.ok);
        assert!(image_check.detail.contains("must pull"));
        assert!(image_check.detail.contains("never runs without it"));
    }
}
