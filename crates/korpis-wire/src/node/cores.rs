// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's physical cores offered for dedication (§5.2 of docs/05-scheduling.md). Read strictly
//! and bounded: what an agent reports becomes what the scheduler gives workloads.

use crate::{Result, invalid};
use korpis_core::node::cores::{Core, check};
use korpis_proto::korpis::v1 as pb;

/// The most cores a node reports, and logical CPUs one core holds.
const MOST_CORES: usize = 1024;
const MOST_CPUS: usize = 8;

/// A node's cores as its agent reports them in `field`: at most 1,024, each one to eight logical
/// CPUs, ascending from its id, no CPU twice, and never the core holding CPU 0.
///
/// # Errors
/// The cores break a bound, or one is not a core as [`check`] reads it.
pub fn cores_from_pb(field: &str, v: &[pb::Core]) -> Result<Vec<Core>> {
    if v.len() > MOST_CORES {
        return Err(invalid(field, "at most 1024 cores"));
    }
    if let Some(i) = v.iter().position(|c| c.cpus.is_empty() || c.cpus.len() > MOST_CPUS) {
        return Err(invalid(&format!("{field}[{i}]"), "one to eight logical CPUs"));
    }
    let cores: Vec<Core> =
        v.iter().map(|c| Core { id: c.id, cpus: c.cpus.clone(), numa: c.numa }).collect();
    check(&cores).map_err(|why| invalid(field, why))?;
    Ok(cores)
}

/// The wire form of a node's core.
#[must_use]
pub fn core_to_pb(c: &Core) -> pb::Core {
    pb::Core { id: c.id, cpus: c.cpus.clone(), numa: c.numa, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(cpus: &[u32]) -> pb::Core {
        pb::Core {
            id: cpus.first().copied().unwrap_or_default(),
            cpus: cpus.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn a_reported_core_reads_back_as_it_was_and_a_bad_set_is_refused() {
        let read = cores_from_pb("c", &[core(&[1, 13])]).unwrap();
        assert_eq!(read.iter().map(core_to_pb).collect::<Vec<_>>(), [core(&[1, 13])]);
        assert!(cores_from_pb("c", &[core(&[])]).is_err(), "no CPU");
        assert!(cores_from_pb("c", &[core(&[0, 12])]).is_err(), "CPU 0's core");
        assert!(cores_from_pb("c", &[core(&[1, 13]), core(&[2, 13])]).is_err(), "a CPU twice");
    }
}
