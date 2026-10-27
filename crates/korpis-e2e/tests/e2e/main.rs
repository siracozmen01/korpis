// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node end to end: the control plane (`scripts/dev-pg.sh up`), the development registry
//! (`scripts/dev-registry.sh up`), and this machine's containerd. The node tests need root and
//! containerd, and are skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod backups;
mod block_storage;
mod conformance;
mod console;
mod containers;
mod cores;
mod dedicated;
mod dependencies;
mod devices;
mod edge_ingress;
mod edges;
mod exec_probe;
mod extension;
mod files;
mod fs_target;
mod guests;
mod health;
mod heartbeat;
mod helper;
mod image;
mod ingress;
mod ingress_features;
mod install;
mod mesh;
mod microvm;
mod microvm_backup;
mod microvm_exec;
mod microvm_files;
mod microvm_heartbeat;
mod microvm_install;
mod microvm_restore;
mod named;
mod network;
mod nightly;
mod node;
mod nodes;
mod oci;
mod programs;
mod query;
mod recipes;
mod replicas;
mod runs;
mod serving;
mod sftp;
mod shared_storage;
mod snapshots;
mod stopping;
mod support;
