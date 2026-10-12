// SPDX-License-Identifier: AGPL-3.0-or-later

//! Records the source revision every Korpis binary was built from, so that `korpis version`
//! and the web client's `/about/source` can name it (§2 of `docs/19-governance.md`).

use std::process::Command;

fn main() {
    let revision = std::env::var("KORPIS_SOURCE_REVISION").ok().unwrap_or_else(|| {
        Command::new("git")
            .args(["describe", "--always", "--dirty", "--abbrev=40"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map_or_else(
                || "unknown".to_owned(),
                |o| String::from_utf8_lossy(&o.stdout).trim().to_owned(),
            )
    });
    println!("cargo:rustc-env=KORPIS_SOURCE_REVISION={revision}");
    println!("cargo:rerun-if-env-changed=KORPIS_SOURCE_REVISION");
    for path in ["../../.git/HEAD", "../../.git/index"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
