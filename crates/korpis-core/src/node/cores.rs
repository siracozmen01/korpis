// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's physical cores offered for dedication (§5.2 of docs/05-scheduling.md): each the
//! logical CPUs that share one core's execution units, as the kernel's topology says, offered
//! whole. A core carries no record of who holds it; placements do.

/// A physical core of a node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Core {
    /// The lowest logical CPU of the core; unique on its node.
    pub id: u32,
    /// Its logical CPUs, ascending, `id` first.
    pub cpus: Vec<u32>,
    /// The NUMA node its CPUs belong to.
    pub numa: u32,
}

/// The millicores a logical CPU counts against a node's allocatable CPU.
pub const MILLIS_PER_CPU: u64 = 1000;

impl Core {
    /// What the core takes from its node's allocatable CPU when held: every logical CPU of it.
    #[must_use]
    pub fn millis(&self) -> u64 {
        u64::try_from(self.cpus.len()).unwrap_or(u64::MAX).saturating_mul(MILLIS_PER_CPU)
    }
}

/// Whether `cores` is a node's set as it may report it: each core's CPUs ascending and its id the
/// first of them, no CPU in two cores, no core twice, and none holding CPU 0, which is never
/// offered.
///
/// # Errors
/// What is wrong with the first core that is.
pub fn check(cores: &[Core]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for c in cores {
        let ascending = c.cpus.windows(2).all(|w| w.first() < w.last());
        if c.cpus.first() != Some(&c.id) || !ascending {
            return Err(format!("core {}: its CPUs ascending, the first its id", c.id));
        }
        if c.cpus.contains(&0) {
            return Err(format!("core {}: the core holding CPU 0 is never offered", c.id));
        }
        if let Some(cpu) = c.cpus.iter().find(|cpu| !seen.insert(**cpu)) {
            return Err(format!("core {}: CPU {cpu} is another core's", c.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(cpus: &[u32]) -> Core {
        Core { id: cpus.first().copied().unwrap_or_default(), cpus: cpus.to_vec(), numa: 0 }
    }

    #[test]
    fn a_core_takes_every_logical_cpu_of_it_and_a_set_names_each_cpu_once() {
        assert_eq!(core(&[1, 13]).millis(), 2000);
        assert!(check(&[core(&[1, 13]), core(&[2, 14])]).is_ok());
        assert!(check(&[core(&[0, 12])]).is_err(), "CPU 0's core");
        assert!(check(&[core(&[1, 13]), core(&[13, 14])]).is_err(), "a CPU twice");
        assert!(check(&[Core { id: 2, cpus: vec![1, 2], numa: 0 }]).is_err(), "id not first");
        assert!(check(&[core(&[3, 1])]).is_err(), "not ascending");
    }
}
