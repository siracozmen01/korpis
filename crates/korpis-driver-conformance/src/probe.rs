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

/// Exits 0 once a `SERVE` or a `STUBBORN` beside it takes the terminating signal.
pub const SERVING: [&str; 2] = ["/probe", "serving"];
