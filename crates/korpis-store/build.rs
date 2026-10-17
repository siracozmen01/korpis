// SPDX-License-Identifier: AGPL-3.0-or-later

//! `sqlx::migrate!` embeds the migrations when the crate compiles, and cargo does not know it
//! read them: without this, a migration added to the directory is left out of every build until
//! some source file changes, and the binary installs a schema older than the one in the tree.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
