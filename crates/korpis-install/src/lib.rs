// SPDX-License-Identifier: AGPL-3.0-or-later

//! The install worker's work (§4 of docs/09-recipes.md): a recipe's install actions, run in
//! order beneath the workload's volumes. Every path is resolved by the kernel beneath the volume
//! it lies in (`openat2` with `RESOLVE_BENEATH`, no magic links, no crossing into another mount),
//! never by inspecting the string, so a link the tenant planted can make a verb fail and can never
//! make it write anywhere else (Rule K-1). Every verb states its idempotence: a file is written
//! whole beside its target and renamed over it, an archive is unpacked into a fresh tree swapped
//! for the old one, and progress is reported after each action, so an install resumes at the
//! action that failed.
//!
//! The confinement this runs inside (its own mount namespace holding only the workload's volumes,
//! Landlock, no network, few capabilities) is the worker binary's; this library is what it runs.

pub mod archive;
pub mod beneath;
mod files;
pub mod run;
mod zstd;

pub use beneath::Volumes;
pub use run::{Content, Failed, run};
