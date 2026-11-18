//! The admission authority of the lifecycle server.
//!
//! The server is the ONLY owner of the slots and of the aggregate budget
//! of environments: the rule of the slots lives in `hodei-mh-budget` (so
//! the server and the clients that predict its behaviour cannot drift
//! apart), and the coordinator keeps the budgets of the goals. There is no
//! second ledger of environments.
//!
//! The authority counts BOTH the environments that exist and the creations
//! that are in flight: a reservation is taken BEFORE any effect, and it is
//! released when the creation fails, when it is abandoned, or when the
//! environment is released.

use std::collections::BTreeMap;

/// What one environment takes of the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EnvironmentResources {
    /// The slots of the host: every environment takes one.
    pub slots: usize,
    pub memory_bytes: i64,
    pub nano_cpus: i64,
}

/// The budget the environments may take. `None` means unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdmissionBudget {
    pub slots: Option<usize>,
    pub memory_bytes: Option<i64>,
    pub nano_cpus: Option<i64>,
}

/// The dimension of the budget that is exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDimension {
    Slots,
    Memory,
    Cpu,
}

impl BudgetDimension {
    pub fn as_str(&self) -> &'static str {
        match self {
            BudgetDimension::Slots => "slots",
            BudgetDimension::Memory => "memory",
            BudgetDimension::Cpu => "cpu",
        }
    }
}

/// The refusal to admit an environment. Nothing was created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionRefusal {
    pub dimension: BudgetDimension,
    pub message: String,
}

/// Why a reservation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionError {
    /// The budget does not admit the environment.
    Exhausted(AdmissionRefusal),
    /// The environment is already accounted for: a creation never reuses
    /// the identity of another one.
    AlreadyAccounted(String),
}

/// The state of one environment in the authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryState {
    /// The creation is in flight: the resources are taken, but no
    /// environment exists yet.
    Reserved,
    /// The environment exists.
    Committed,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    resources: EnvironmentResources,
    state: EntryState,
    /// When the environment became one: the reconciliations that started
    /// before it do not touch it.
    committed_at: u64,
}

/// The authority: the single place that decides whether an environment may
/// be created, and what it takes of the host.
#[derive(Debug)]
pub struct AdmissionAuthority {
    budget: AdmissionBudget,
    entries: BTreeMap<String, Entry>,
    /// The order in which the environments became real.
    sequence: u64,
}

impl AdmissionAuthority {
    pub fn new(budget: AdmissionBudget) -> Self {
        Self {
            budget,
            entries: BTreeMap::new(),
            sequence: 0,
        }
    }

    /// The budget this authority enforces.
    pub fn budget(&self) -> AdmissionBudget {
        self.budget
    }

    /// The order of the environments that became real.
    ///
    /// A reconciliation reads it BEFORE it observes the daemon: the
    /// environments that become real after that moment are not affected by
    /// an observation that could not have seen them.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Take the resources of an environment that is about to be created.
    ///
    /// The decision and the reservation happen together: a caller that
    /// gets an `Ok` owns the resources, and a caller that gets a refusal
    /// owns nothing.
    pub fn reserve(
        &mut self,
        sandbox_id: &str,
        resources: EnvironmentResources,
    ) -> Result<(), AdmissionError> {
        if self.entries.contains_key(sandbox_id) {
            return Err(AdmissionError::AlreadyAccounted(sandbox_id.to_string()));
        }
        let taken = self.occupancy();
        if let Some(slots) = self.budget.slots
            && budget::ledger::admissible_creates(taken.slots, slots) == 0
        {
            return Err(AdmissionError::Exhausted(AdmissionRefusal {
                dimension: BudgetDimension::Slots,
                message: format!(
                    "global environment budget exhausted: {} of {slots} slots in use; \
                     no environment is created and the caller must WAIT (no silent admission)",
                    taken.slots
                ),
            }));
        }
        if let Some(limit) = self.budget.memory_bytes
            && taken.memory_bytes.saturating_add(resources.memory_bytes) > limit
        {
            return Err(AdmissionError::Exhausted(AdmissionRefusal {
                dimension: BudgetDimension::Memory,
                message: format!(
                    "global memory budget exhausted: {} bytes in use, {} bytes requested, \
                     {limit} bytes allowed; no environment is created",
                    taken.memory_bytes, resources.memory_bytes
                ),
            }));
        }
        if let Some(limit) = self.budget.nano_cpus
            && taken.nano_cpus.saturating_add(resources.nano_cpus) > limit
        {
            return Err(AdmissionError::Exhausted(AdmissionRefusal {
                dimension: BudgetDimension::Cpu,
                message: format!(
                    "global cpu budget exhausted: {} nano-cpus in use, {} nano-cpus requested, \
                     {limit} nano-cpus allowed; no environment is created",
                    taken.nano_cpus, resources.nano_cpus
                ),
            }));
        }
        self.entries.insert(
            sandbox_id.to_string(),
            Entry {
                resources,
                state: EntryState::Reserved,
                committed_at: 0,
            },
        );
        Ok(())
    }

    /// The environment exists: the reservation becomes a commitment. The
    /// resources do not change — the environment was already counted.
    pub fn commit(&mut self, sandbox_id: &str) {
        self.sequence += 1;
        let sequence = self.sequence;
        if let Some(entry) = self.entries.get_mut(sandbox_id) {
            entry.state = EntryState::Committed;
            entry.committed_at = sequence;
        }
    }

    /// The environment is gone, or never came to be: its resources are
    /// free again. Returns whether something was released.
    pub fn release(&mut self, sandbox_id: &str) -> bool {
        self.entries.remove(sandbox_id).is_some()
    }

    /// Reconcile the authority with what the daemon reports.
    ///
    /// The environments that exist are the observed ones: the ones that
    /// are gone stop counting, and the ones that appeared start counting.
    /// The creations IN FLIGHT are not affected: they are not visible to
    /// the daemon yet, and a reconciliation must not hand their resources
    /// to somebody else.
    ///
    /// `horizon` is the sequence read BEFORE the observation started: the
    /// environments that became real after it are kept, because the
    /// observation could not have seen them.
    pub fn reconcile(&mut self, observed: &[(String, EnvironmentResources)], horizon: u64) {
        let observed: BTreeMap<&str, EnvironmentResources> = observed
            .iter()
            .map(|(id, resources)| (id.as_str(), *resources))
            .collect();
        self.entries.retain(|id, entry| {
            entry.state == EntryState::Reserved
                || entry.committed_at > horizon
                || observed.contains_key(id.as_str())
        });
        for (id, resources) in observed {
            self.entries.insert(
                id.to_string(),
                Entry {
                    resources,
                    state: EntryState::Committed,
                    committed_at: self.sequence,
                },
            );
        }
    }

    /// What the accounted environments take of the host: the existing ones
    /// AND the creations in flight.
    pub fn occupancy(&self) -> EnvironmentResources {
        self.entries
            .values()
            .fold(EnvironmentResources::default(), |acc, entry| {
                EnvironmentResources {
                    slots: acc.slots + entry.resources.slots,
                    memory_bytes: acc
                        .memory_bytes
                        .saturating_add(entry.resources.memory_bytes),
                    nano_cpus: acc.nano_cpus.saturating_add(entry.resources.nano_cpus),
                }
            })
    }

    /// How many environments are accounted for.
    pub fn environments(&self) -> usize {
        self.entries.len()
    }

    /// How many creations are in flight.
    pub fn creations_in_flight(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.state == EntryState::Reserved)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resources(slots: usize, memory_bytes: i64, nano_cpus: i64) -> EnvironmentResources {
        EnvironmentResources {
            slots,
            memory_bytes,
            nano_cpus,
        }
    }

    #[test]
    fn a_reservation_takes_the_resources_before_the_environment_exists() {
        let mut authority = AdmissionAuthority::new(AdmissionBudget {
            slots: Some(1),
            ..AdmissionBudget::default()
        });
        authority
            .reserve("sbx-1", resources(1, 1024, 1_000_000_000))
            .expect("the first environment is admitted");
        assert_eq!(authority.environments(), 1);
        assert_eq!(authority.creations_in_flight(), 1);
        // The environment does not exist yet, and it still counts.
        let refused = authority
            .reserve("sbx-2", resources(1, 1024, 1_000_000_000))
            .expect_err("the slot is taken");
        assert!(matches!(
            refused,
            AdmissionError::Exhausted(AdmissionRefusal {
                dimension: BudgetDimension::Slots,
                ..
            })
        ));
        // The commitment does not change what is accounted.
        authority.commit("sbx-1");
        assert_eq!(authority.occupancy().slots, 1);
        assert_eq!(authority.creations_in_flight(), 0);
    }

    #[test]
    fn the_resources_of_a_failed_creation_are_free_again() {
        let mut authority = AdmissionAuthority::new(AdmissionBudget {
            slots: Some(1),
            ..AdmissionBudget::default()
        });
        authority
            .reserve("sbx-1", resources(1, 1024, 1_000_000_000))
            .expect("admitted");
        assert!(authority.release("sbx-1"));
        authority
            .reserve("sbx-2", resources(1, 1024, 1_000_000_000))
            .expect("the slot is free again");
    }

    #[test]
    fn the_memory_and_the_cpu_budgets_are_enforced_with_the_effective_limits() {
        let mut authority = AdmissionAuthority::new(AdmissionBudget {
            slots: None,
            memory_bytes: Some(2048),
            nano_cpus: Some(2_000_000_000),
        });
        authority
            .reserve("sbx-1", resources(1, 1024, 1_000_000_000))
            .expect("admitted");
        let refused = authority
            .reserve("sbx-2", resources(1, 2048, 1_000_000_000))
            .expect_err("the memory budget is exhausted");
        assert!(matches!(
            refused,
            AdmissionError::Exhausted(AdmissionRefusal {
                dimension: BudgetDimension::Memory,
                ..
            })
        ));
        let refused = authority
            .reserve("sbx-3", resources(1, 1024, 2_000_000_000))
            .expect_err("the cpu budget is exhausted");
        assert!(matches!(
            refused,
            AdmissionError::Exhausted(AdmissionRefusal {
                dimension: BudgetDimension::Cpu,
                ..
            })
        ));
    }

    #[test]
    fn the_reconciliation_forgets_what_is_gone_and_keeps_what_is_in_flight() {
        let mut authority = AdmissionAuthority::new(AdmissionBudget {
            slots: Some(2),
            ..AdmissionBudget::default()
        });
        authority
            .reserve("sbx-1", resources(1, 1024, 1_000_000_000))
            .expect("admitted");
        authority.commit("sbx-1");
        authority
            .reserve("sbx-2", resources(1, 1024, 1_000_000_000))
            .expect("admitted");
        // The daemon reports ONE environment: the other one is gone.
        let horizon = authority.sequence();
        authority.reconcile(
            &[("sbx-1".to_string(), resources(1, 1024, 1_000_000_000))],
            horizon,
        );
        assert_eq!(authority.environments(), 2);
        assert_eq!(authority.creations_in_flight(), 1);
        // The creation in flight still holds its slot.
        let refused = authority
            .reserve("sbx-3", resources(1, 1024, 1_000_000_000))
            .expect_err("both slots are taken");
        assert!(matches!(
            refused,
            AdmissionError::Exhausted(AdmissionRefusal {
                dimension: BudgetDimension::Slots,
                ..
            })
        ));
    }

    #[test]
    fn a_reconciled_server_counts_the_environments_that_already_exist() {
        // What a RESTART sees: the reservations of the previous process are
        // gone, and the environments it left behind are the ones the daemon
        // reports.
        let mut authority = AdmissionAuthority::new(AdmissionBudget {
            slots: Some(2),
            ..AdmissionBudget::default()
        });
        let horizon = authority.sequence();
        authority.reconcile(
            &[
                ("sbx-1".to_string(), resources(1, 1024, 1_000_000_000)),
                ("sbx-2".to_string(), resources(1, 1024, 1_000_000_000)),
            ],
            horizon,
        );
        assert_eq!(authority.environments(), 2);
        let refused = authority
            .reserve("sbx-3", resources(1, 1024, 1_000_000_000))
            .expect_err("the host already carries the whole budget");
        assert!(matches!(
            refused,
            AdmissionError::Exhausted(AdmissionRefusal {
                dimension: BudgetDimension::Slots,
                ..
            })
        ));
    }

    #[test]
    fn the_same_environment_is_never_accounted_twice() {
        let mut authority = AdmissionAuthority::new(AdmissionBudget::default());
        authority
            .reserve("sbx-1", resources(1, 1024, 1_000_000_000))
            .expect("admitted");
        let error = authority
            .reserve("sbx-1", resources(1, 1024, 1_000_000_000))
            .expect_err("the identity is already accounted");
        assert_eq!(error, AdmissionError::AlreadyAccounted("sbx-1".to_string()));
    }
}
