//! Resource vectors (GC-12): CPU/RAM/disk as one typed vector, and the host
//! capacity with a CONFIGURABLE reserve — the advertised targets (48 CPU /
//! 64 GiB) are hypotheses, never guarantees (AC-4).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceVector {
    /// Logical CPUs (quota, not reserved cores — quotas and reservations
    /// are different things and this vector only speaks quotas).
    pub cpus: f64,
    pub memory_gib: f64,
    pub disk_gib: f64,
}

impl ResourceVector {
    pub fn new(cpus: f64, memory_gib: f64, disk_gib: f64) -> Self {
        Self {
            cpus,
            memory_gib,
            disk_gib,
        }
    }

    /// Component-wise sum/difference for aggregation.
    pub fn saturating_add(&self, other: &Self) -> Self {
        Self {
            cpus: self.cpus + other.cpus,
            memory_gib: self.memory_gib + other.memory_gib,
            disk_gib: self.disk_gib + other.disk_gib,
        }
    }

    pub fn saturating_sub(&self, other: &Self) -> Self {
        Self {
            cpus: (self.cpus - other.cpus).max(0.0),
            memory_gib: (self.memory_gib - other.memory_gib).max(0.0),
            disk_gib: (self.disk_gib - other.disk_gib).max(0.0),
        }
    }

    pub fn fits_within(&self, available: &Self) -> bool {
        self.cpus <= available.cpus
            && self.memory_gib <= available.memory_gib
            && self.disk_gib <= available.disk_gib
    }
}

/// Host capacity: measured totals minus a configurable protective reserve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostCapacity {
    /// Measured host totals (AC-4: measured, not assumed).
    pub total: ResourceVector,
    /// Protective margin the host keeps for itself (never handed to
    /// sandboxes). Configurable per host.
    pub reserve: ResourceVector,
}

impl HostCapacity {
    /// What the budget may actually hand out.
    pub fn usable(&self) -> ResourceVector {
        self.total.saturating_sub(&self.reserve)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_subtracts_the_configurable_reserve() {
        let host = HostCapacity {
            total: ResourceVector::new(64.0, 62.5, 900.0),
            reserve: ResourceVector::new(8.0, 8.0, 100.0),
        };
        let usable = host.usable();
        assert_eq!(usable.cpus, 56.0);
        assert_eq!(usable.memory_gib, 54.5);
        // A 48-CPU target would fit here, but it stays a hypothesis: the
        // budget only ever speaks about MEASURED totals minus reserve.
        assert!(!ResourceVector::new(57.0, 1.0, 1.0).fits_within(&usable));
    }

    #[test]
    fn reserve_never_produces_negative_usable() {
        let host = HostCapacity {
            total: ResourceVector::new(4.0, 8.0, 100.0),
            reserve: ResourceVector::new(8.0, 16.0, 200.0),
        };
        let usable = host.usable();
        assert_eq!(usable.cpus, 0.0);
        assert_eq!(usable.memory_gib, 0.0);
        assert_eq!(usable.disk_gib, 0.0);
    }
}
