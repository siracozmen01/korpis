// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis login` on a terminal, waiting for a person in the browser to approve it (§9.4 of
//! docs/08-identity.md), and what it said once they did.

use crate::support::ControlPlane;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, BufReader};

/// A `korpis login` waiting to be approved.
pub struct Login {
    child: tokio::process::Child,
    /// The address it says to approve it at.
    pub at: String,
    rest: tokio::task::JoinHandle<String>,
}

/// `korpis login` from the binary `korpis`, keeping nothing, run to read the caller's profile
/// once signed in, its sessions under `config`. What it writes to its standard error is read to
/// the end as it is written: a pipe closed under it would fail its next write, and with it the
/// command.
pub async fn logging_in(cp: &ControlPlane, korpis: &Path, config: &Path) -> Login {
    std::fs::create_dir_all(config).unwrap();
    let mut child = tokio::process::Command::new(korpis)
        .args(["login", "--no-store", "--", "call", "ProfileService/GetProfile"])
        .env("KORPIS_SERVER", &cp.url)
        .env("KORPIS_CA", cp.data.tls().join("ca.pem"))
        .env_remove("KORPIS_TOKEN")
        .env("KORPIS_KEYRING", "off")
        .env("KORPIS_CONFIG_DIR", config)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // "To sign in, open <uri> and enter the code <code>", then "or open <uri with the code>".
    let mut told = BufReader::new(child.stderr.take().unwrap()).lines();
    let read = async {
        while let Some(line) = told.next_line().await.unwrap() {
            if let Some(at) = line.strip_prefix("or open ") {
                return at.to_owned();
            }
        }
        panic!("korpis login named no address to approve it at");
    };
    let at = tokio::time::timeout(Duration::from_secs(30), read).await.unwrap();
    let rest = tokio::spawn(async move {
        let mut rest = String::new();
        while let Ok(Some(line)) = told.next_line().await {
            rest.push_str(&line);
            rest.push('\n');
        }
        rest
    });
    Login { child, at, rest }
}

impl Login {
    /// What it wrote to its standard output, once it ended having been approved; it fails the
    /// test if it did not end within thirty seconds or ended in failure.
    pub async fn said(self) -> String {
        let ended = tokio::time::timeout(Duration::from_secs(30), self.child.wait_with_output());
        let ended = ended.await.expect("korpis login ended once approved").unwrap();
        let said = String::from_utf8_lossy(&ended.stdout).into_owned();
        let told = self.rest.await.unwrap();
        assert!(ended.status.success(), "korpis login, {}: {said} {told}", ended.status);
        said
    }
}
