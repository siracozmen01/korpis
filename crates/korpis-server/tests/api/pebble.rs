// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pebble, Let's Encrypt's test ACME server, as scripts/test-tools.sh installs it, run for one
//! test on ports of its own with an authority made for it. A test without it says so and does not
//! run.

use korpis_server::acme::AcmeConfig;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// A running Pebble.
pub struct Pebble {
    child: Child,
    _dir: tempfile::TempDir,
    /// Its directory's URL.
    pub directory: String,
    /// The authority its own certificate is trusted by.
    pub ca: PathBuf,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Pebble {
    /// Pebble, every challenge taken as valid when `always_valid`, its HTTP-01 validation
    /// otherwise made on port `http_port`; `None` where it is not installed.
    pub async fn start(always_valid: bool, http_port: u16) -> Option<Self> {
        let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.tools/bin/pebble");
        if !bin.exists() {
            #[expect(clippy::print_stderr, reason = "a skipped test says why")]
            {
                eprintln!("pebble is not installed (scripts/test-tools.sh): not run");
            }
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let made = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into(), "localhost".into()])
            .unwrap();
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        std::fs::write(&cert, made.cert.pem()).unwrap();
        std::fs::write(&key, made.signing_key.serialize_pem()).unwrap();
        let (listen, manage) = (free_port(), free_port());
        let config = serde_json::json!({"pebble": {
            "listenAddress": format!("127.0.0.1:{listen}"),
            "managementListenAddress": format!("127.0.0.1:{manage}"),
            "certificate": cert, "privateKey": key,
            "httpPort": http_port, "tlsPort": free_port(),
            "ocspResponderURL": "", "externalAccountBindingRequired": false,
            "retryAfter": {"authz": 1, "order": 1}, "keyAlgorithm": "ecdsa",
            "profiles": {"default": {"description": "test", "validityPeriod": 7_776_000}},
        }});
        let path = dir.path().join("pebble.json");
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let child = Command::new(bin)
            .arg("-config")
            .arg(&path)
            .env("PEBBLE_VA_ALWAYS_VALID", if always_valid { "1" } else { "0" })
            .env("PEBBLE_VA_NOSLEEP", "1")
            .env("PEBBLE_WFE_NONCEREJECT", "0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let directory = format!("https://127.0.0.1:{listen}/dir");
        // Made at once, so it is stopped however the wait below ends.
        let pebble = Self { child, _dir: dir, directory, ca: cert };
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", listen)).is_ok() {
                return Some(pebble);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("pebble did not listen");
    }

    /// The control plane's settings for this directory, its terms agreed.
    pub fn config(&self) -> AcmeConfig {
        AcmeConfig {
            directory: self.directory.clone(),
            ca: Some(self.ca.clone()),
            contact: vec!["mailto:operator@example.com".into()],
            agreed: true,
        }
    }
}

impl Drop for Pebble {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
