// SPDX-License-Identifier: Apache-2.0

//! What the probe is asked to do, as the entry of the object it runs as
//! (`korpis-conformance-probe`).

/// Runs until the terminating signal, then exits 0.
pub const SERVE: [&str; 2] = ["/probe", "serve"];

/// Runs until it is killed: the terminating signal is taken and ignored.
pub const STUBBORN: [&str; 2] = ["/probe", "stubborn"];

/// Exits at once with status 3.
pub const EXIT_3: [&str; 3] = ["/probe", "exit", "3"];

/// Takes memory until the kernel kills it for exceeding its limit.
pub const ALLOC: [&str; 2] = ["/probe", "alloc"];

/// Starts threads until one is refused, and says how many.
pub const THREADS: [&str; 2] = ["/probe", "threads"];

/// Writes in the volume mounted at `/data` until a write is refused, and says how much and why.
pub const FILL: [&str; 3] = ["/probe", "fill", "/data"];

/// Keeps four threads busy, more than the processor time an object may use.
pub const SPIN: [&str; 3] = ["/probe", "spin", "4"];

/// Says the logical CPUs it may run on, as the kernel lists them.
pub const CPUS: [&str; 2] = ["/probe", "cpus"];

/// Exits 0 once a `SERVE` or a `STUBBORN` beside it takes the terminating signal.
pub const SERVING: [&str; 2] = ["/probe", "serving"];

/// Says who it runs as: its ids, capabilities, namespaces, cgroup, limits, working directory, and
/// environment, one line each.
pub const WHOAMI: [&str; 2] = ["/probe", "whoami"];

/// Says who a `SERVE` or a `STUBBORN` beside it runs as, as `WHOAMI` would have said it.
pub const ENTRY: [&str; 2] = ["/probe", "entry"];

/// Says how many processes it sees running `STUBBORN`.
pub const COUNT_STUBBORN: [&str; 3] = ["/probe", "count", "stubborn"];

/// Writes `bytes` bytes, then exits 0.
#[must_use]
pub fn write(bytes: &str) -> [&str; 3] {
    ["/probe", "write", bytes]
}
