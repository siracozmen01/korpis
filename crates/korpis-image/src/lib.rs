// SPDX-License-Identifier: AGPL-3.0-or-later

//! Image drives for the tiers whose root filesystem is a block device (§6 of
//! docs/04-runtimes.md): an OCI image from the node's content store, its layers applied one on
//! another and the result made an ext4 filesystem. The layers are a publisher's bytes, so they are
//! read by a process holding no privilege: `korpis-image-build` runs as two, the first reading the
//! content store and checking each layer against its digest as it streams it, the second applying
//! the layers and making the filesystem in a user namespace whose root is a range of ids the
//! caller names, so the image's owners are the workload's own ids from 0 and nothing it holds was
//! ever made by host root. The boot drive every guest shares, holding Korpis's own first process
//! and nothing a tenant wrote, is made directly ([`boot`]).

#![warn(missing_docs)]

pub mod boot;
pub mod build;
pub mod layer;
pub mod source;
pub mod stream;
#[cfg(feature = "testing")]
pub mod testing;
pub mod tree;
