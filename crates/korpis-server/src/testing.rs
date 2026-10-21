// SPDX-License-Identifier: AGPL-3.0-or-later

//! Test support, behind the `testing` feature: Let's Encrypt's test ACME server, Pebble, and its
//! challenge test server's DNS, as scripts/test-tools.sh installs them, each run for one test on
//! ports of its own and stopped when dropped; and the installation every harness serves.

mod pebble;

pub use pebble::{ChallengeDns, Pebble, PebbleOptions, TestingError};

/// How long after a reachability probe that measured nothing the node is probed again, which a
/// test waiting for a measurement must wait past (§9 of docs/07-networking.md).
pub const PROBE_RETRY: std::time::Duration = crate::reachability::RETRY;

use crate::data::DataDir;
use crate::install::{InstallConfig, RootHolder, install};
use korpis_app::install::Installation;
use korpis_core::tenancy::Name;

/// Installs the root organization `acme` into the database `owner_url` names, its root grant
/// held by a token and its certificate valid for `localhost`, to be served by `login`, a login
/// of the test's own: `korpis_server`, which installation would make, would be every concurrent
/// test's.
///
/// # Panics
/// Installation failed.
#[expect(clippy::expect_used, reason = "a harness that cannot install fails its test loudly")]
pub async fn install_acme(owner_url: &str, data: &DataDir, login: &str) -> Installation {
    install(&InstallConfig {
        data: data.clone(),
        owner_url: owner_url.to_owned(),
        organization: Name::parse("acme").expect("a name"),
        tls_names: vec!["localhost".to_owned()],
        root: RootHolder::Token,
        first_node: None,
        backup_target: None,
        database_url: Some(login.to_owned()),
    })
    .await
    .expect("install")
    .installation
}
