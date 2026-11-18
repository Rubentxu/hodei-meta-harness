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
    /// Whether this check BLOCKS the run. An informational check (e.g. a
    /// user image that is not pinned) is reported without blocking.
    pub blocking: bool,
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
    // An UNPINNED user image has no digest to mismatch: it is present or
    // it is not. A PINNED profile still demands an exact digest match.
    let pinned = spec.image.digest.is_some();
    let (digest_matches, image_check) = match (pinned, image_local) {
        (true, Some(local_digest)) => {
            let matches = spec.image.digest.as_deref() == Some(local_digest);
            let detail = match spec.image.digest.as_deref() {
                Some(expected) => format!(
                    "pinned {expected}; local {local_digest}{}",
                    if matches { "" } else { " (MISMATCH)" }
                ),
                None => unreachable!("pinned profiles always carry a digest"),
            };
            (
                matches,
                DoctorCheck {
                    name: "pinned-image-local".into(),
                    ok: matches,
                    detail,
                    blocking: true,
                },
            )
        }
        (true, None) => (
            false,
            DoctorCheck {
                name: "pinned-image-local".into(),
                ok: false,
                detail: format!(
                    "{} not present locally — the docker daemon must pull it first (the profile never runs without it)",
                    spec.image.full_reference()
                ),
                blocking: true,
            },
        ),
        (false, Some(_)) => (
            true,
            DoctorCheck {
                name: "pinned-image-local".into(),
                ok: true,
                detail: format!(
                    "user image {} present locally; NOT pinned to a digest (reproducibility depends on the registry tag)",
                    spec.image.full_reference()
                ),
                blocking: true,
            },
        ),
        (false, None) => (
            false,
            DoctorCheck {
                name: "pinned-image-local".into(),
                ok: false,
                detail: format!(
                    "user image {} not present locally — the docker daemon must pull it first",
                    spec.image.full_reference()
                ),
                blocking: true,
            },
        ),
    };
    let checks = vec![
        DoctorCheck {
            name: "profile-pinned".into(),
            ok: pinned,
            detail: if pinned {
                spec.image.full_reference()
            } else {
                format!(
                    "{} is a USER image NOT pinned to a digest — reproducibility is not guaranteed (informational)",
                    spec.image.full_reference()
                )
            },
            // Informational: a user image is accepted, but the fact that it
            // is unpinned is reported.
            blocking: false,
        },
        image_check,
        DoctorCheck {
            name: "restricted-egress".into(),
            ok: egress_reachable,
            // State exactly WHAT was measured: the proxy answered a
            // connection attempt. Whether the sandbox network is
            // restricted is a separate, explicit fact.
            detail: if egress_reachable {
                "egress proxy reachable (TCP connect succeeded)".into()
            } else {
                "egress proxy NOT reachable — builds needing the network would refuse to run".into()
            },
            blocking: true,
        },
        DoctorCheck {
            name: "sandbox-egress-isolation".into(),
            // The host-side proxy check says nothing about what the
            // sandbox may reach. Without a measurement FROM INSIDE a
            // sandbox, isolation is UNKNOWN, not proven.
            ok: false,
            detail: "not measured: isolation must be verified from inside a sandbox (this check reports unknown, not a pass)".into(),
            blocking: false,
        },
        DoctorCheck {
            name: "opensandbox-api".into(),
            ok: opensandbox_ready,
            detail: if opensandbox_ready {
                "lifecycle API answers".into()
            } else {
                "lifecycle API unreachable — no sandbox can be created".into()
            },
            blocking: true,
        },
        DoctorCheck {
            name: "subscription-limitations".into(),
            // A limitation is reported, not judged: it becomes blocking
            // only when it forbids what the run needs.
            ok: true,
            detail: format!("tier {}: {}", subscription.tier, subscription.limitation),
            blocking: false,
        },
    ];
    let input = GateInput {
        image_available: image_local.is_some(),
        digest_matches,
        egress_available: egress_reachable,
        opensandbox_available: opensandbox_ready,
    };
    (checks, input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{ImagePin, rust_profile, user_profile};

    fn subscription() -> crate::credentials::SubscriptionNote {
        crate::credentials::SubscriptionNote {
            tier: "none".into(),
            limitation: "no credentials configured".into(),
        }
    }

    #[test]
    fn doctor_reports_every_dimension_and_never_claims_all_ok_when_something_is_missing() {
        let spec = rust_profile(ImagePin::pinned("rust:1-bookworm", "sha256:correct"));
        let (checks, input) = checks_for(&spec, Some("sha256:WRONG"), false, true, &subscription());
        assert_eq!(checks.len(), 6);
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
        let (checks, _) = checks_for(&spec, None, true, true, &subscription());
        let image_check = checks
            .iter()
            .find(|c| c.name == "pinned-image-local")
            .expect("check");
        assert!(!image_check.ok);
        assert!(image_check.detail.contains("must pull"));
        assert!(image_check.detail.contains("never runs without it"));
    }

    #[test]
    fn a_user_image_present_locally_is_accepted_and_reported_as_unpinned() {
        let (spec, _note) = user_profile("user", ImagePin::user("my/app:1.2"), "user toolchain");
        let (checks, input) =
            checks_for(&spec, Some("sha256:whatever"), true, true, &subscription());
        // The gate does NOT refuse: an unpinned image has no digest to
        // mismatch.
        assert!(
            matches!(
                crate::gate::decide(&input),
                crate::gate::GateDecision::Proceed
            ),
            "a present user image must not be refused for a digest it never had"
        );
        let pinned_check = checks
            .iter()
            .find(|c| c.name == "profile-pinned")
            .expect("check");
        assert!(!pinned_check.ok, "the fact that it is unpinned is reported");
        assert!(!pinned_check.blocking, "but it does not block the run");
        assert!(pinned_check.detail.contains("NOT pinned"));
        let image_check = checks
            .iter()
            .find(|c| c.name == "pinned-image-local")
            .expect("check");
        assert!(image_check.ok);
    }

    #[test]
    fn a_user_image_absent_locally_still_refuses() {
        let (spec, _note) = user_profile("user", ImagePin::user("my/app:1.2"), "tool");
        let (checks, input) = checks_for(&spec, None, true, true, &subscription());
        assert!(matches!(
            crate::gate::decide(&input),
            crate::gate::GateDecision::Refuse { .. }
        ));
        let image_check = checks
            .iter()
            .find(|c| c.name == "pinned-image-local")
            .expect("check");
        assert!(!image_check.ok);
        assert!(image_check.detail.contains("not present locally"));
    }

    #[test]
    fn the_doctor_never_claims_sandbox_isolation_without_measuring_it() {
        let spec = rust_profile(ImagePin::pinned("rust:1-bookworm", "sha256:r"));
        let (checks, _) = checks_for(&spec, Some("sha256:r"), true, true, &subscription());
        let isolation = checks
            .iter()
            .find(|c| c.name == "sandbox-egress-isolation")
            .expect("check");
        assert!(!isolation.ok, "unmeasured isolation is not a pass");
        assert!(!isolation.blocking, "and it does not silently block either");
        assert!(isolation.detail.contains("not measured"));
    }
}
