//! Budget ledger (AC-1): aggregate admissions against usable capacity.
//! Environments still PROVISIONING count against the budget — a burst of
//! creates cannot oversubscribe the host through the creation window.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::vector::{HostCapacity, ResourceVector};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct SandboxId(pub String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EnvironmentState {
    /// Create request accepted; resources already count.
    Provisioning,
    /// Sandbox running; resources still count.
    Running,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Entry {
    state: EnvironmentState,
    resources: ResourceVector,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum WaitReason {
    #[default]
    CpuExhausted,
    MemoryExhausted,
    DiskExhausted,
    BudgetSuspended,
}

impl std::fmt::Display for WaitReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CpuExhausted => write!(f, "cpu budget exhausted"),
            Self::MemoryExhausted => write!(f, "memory budget exhausted"),
            Self::DiskExhausted => write!(f, "disk budget exhausted"),
            Self::BudgetSuspended => write!(f, "admissions suspended by host pressure"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Admission {
    Admitted,
    Waitlisted { reason: WaitReason },
}

/// The aggregate budget state. Committed + provisioning both count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetLedger {
    entries: BTreeMap<SandboxId, Entry>,
    /// Set by pressure reactions; blocks NEW admissions only.
    suspended: bool,
}

impl Default for BudgetLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl BudgetLedger {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            suspended: false,
        }
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended
    }

    pub fn suspend(&mut self) {
        self.suspended = true;
    }

    pub fn resume(&mut self) {
        self.suspended = false;
    }

    /// Committed total (running + provisioning).
    pub fn committed(&self) -> ResourceVector {
        self.entries
            .values()
            .fold(ResourceVector::new(0.0, 0.0, 0.0), |acc, e| {
                acc.saturating_add(&e.resources)
            })
    }

    pub fn provisioning_count(&self) -> usize {
        self.entries
            .values()
            .filter(|e| e.state == EnvironmentState::Provisioning)
            .count()
    }

    /// AC-1: the decision BEFORE any sandbox is created. Provisioning
    /// environments count, suspension blocks, and an over-budget request
    /// waits without consuming anything.
    pub fn admit(
        &mut self,
        capacity: &HostCapacity,
        id: SandboxId,
        request: ResourceVector,
    ) -> Admission {
        if self.suspended {
            return Admission::Waitlisted {
                reason: WaitReason::BudgetSuspended,
            };
        }
        let available = capacity.usable().saturating_sub(&self.committed());
        let reason = if request.cpus > available.cpus {
            WaitReason::CpuExhausted
        } else if request.memory_gib > available.memory_gib {
            WaitReason::MemoryExhausted
        } else if request.disk_gib > available.disk_gib {
            WaitReason::DiskExhausted
        } else {
            self.entries.insert(
                id,
                Entry {
                    state: EnvironmentState::Provisioning,
                    resources: request,
                },
            );
            return Admission::Admitted;
        };
        Admission::Waitlisted { reason }
    }

    /// The environment finished provisioning (still counted).
    pub fn mark_running(&mut self, id: &SandboxId) -> bool {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.state = EnvironmentState::Running;
            return true;
        }
        false
    }

    /// The environment is gone: its budget is released.
    pub fn release(&mut self, id: &SandboxId) -> bool {
        self.entries.remove(id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity() -> HostCapacity {
        HostCapacity {
            total: ResourceVector::new(8.0, 8.0, 100.0),
            reserve: ResourceVector::new(1.0, 1.0, 10.0),
        }
    }

    #[test]
    fn over_budget_request_waits_and_creates_nothing() {
        let mut ledger = BudgetLedger::new();
        let admission = ledger.admit(
            &capacity(),
            SandboxId("a".into()),
            ResourceVector::new(10.0, 2.0, 5.0),
        );
        assert_eq!(
            admission,
            Admission::Waitlisted {
                reason: WaitReason::CpuExhausted
            }
        );
        // Nothing was consumed: the waitlist happened BEFORE any create.
        assert_eq!(ledger.committed(), ResourceVector::new(0.0, 0.0, 0.0));
    }

    #[test]
    fn provisioning_environments_count_toward_the_budget() {
        let mut ledger = BudgetLedger::new();
        // 7 GiB requested of 7 usable: admitted, still provisioning.
        let a = ledger.admit(
            &capacity(),
            SandboxId("a".into()),
            ResourceVector::new(1.0, 7.0, 5.0),
        );
        assert_eq!(a, Admission::Admitted);
        assert_eq!(ledger.provisioning_count(), 1);
        // A concurrent burst CANNOT sneak through the provisioning window.
        let b = ledger.admit(
            &capacity(),
            SandboxId("b".into()),
            ResourceVector::new(1.0, 1.0, 1.0),
        );
        assert_eq!(
            b,
            Admission::Waitlisted {
                reason: WaitReason::MemoryExhausted
            }
        );
    }

    #[test]
    fn released_budget_is_available_again() {
        let mut ledger = BudgetLedger::new();
        let _ = ledger.admit(
            &capacity(),
            SandboxId("a".into()),
            ResourceVector::new(7.0, 7.0, 5.0),
        );
        ledger.mark_running(&SandboxId("a".into()));
        assert!(ledger.release(&SandboxId("a".into())));
        let again = ledger.admit(
            &capacity(),
            SandboxId("b".into()),
            ResourceVector::new(7.0, 7.0, 5.0),
        );
        assert_eq!(again, Admission::Admitted);
    }

    #[test]
    fn suspension_blocks_admissions_but_not_release() {
        let mut ledger = BudgetLedger::new();
        let _ = ledger.admit(
            &capacity(),
            SandboxId("a".into()),
            ResourceVector::new(1.0, 1.0, 1.0),
        );
        ledger.suspend();
        let blocked = ledger.admit(
            &capacity(),
            SandboxId("b".into()),
            ResourceVector::new(0.1, 0.1, 0.1),
        );
        assert_eq!(
            blocked,
            Admission::Waitlisted {
                reason: WaitReason::BudgetSuspended
            }
        );
        assert!(ledger.release(&SandboxId("a".into())));
        ledger.resume();
        let resumed = ledger.admit(
            &capacity(),
            SandboxId("c".into()),
            ResourceVector::new(0.1, 0.1, 0.1),
        );
        assert_eq!(resumed, Admission::Admitted);
    }
}
