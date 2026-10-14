// SPDX-License-Identifier: AGPL-3.0-or-later

//! MinIO as a plain host process on a free loopback port, with its data in a directory that goes
//! when it does. `scripts/test-tools.sh` installs it.

use rusty_s3::actions::S3Action as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub const USER: &str = "korpis-test";
pub const SECRET: &str = "korpis-test-secret";

/// A running MinIO.
pub struct Minio {
    child: Child,
    _data: tempfile::TempDir,
    /// Where it listens.
    pub endpoint: String,
}

impl Drop for Minio {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.tools/bin/minio")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Minio {
    /// A MinIO serving, or `None` where none is installed.
    pub async fn start() -> Option<Self> {
        if !binary().exists() {
            eprintln!("MinIO is not installed (scripts/test-tools.sh): the S3 tests do not run");
            return None;
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let data = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let (port, console) = (free_port(), free_port());
        let child = Command::new(binary())
            .args(["server", "--quiet", "--address", &format!("127.0.0.1:{port}")])
            .args(["--console-address", &format!("127.0.0.1:{console}")])
            .arg(data.path())
            .env("MINIO_ROOT_USER", USER)
            .env("MINIO_ROOT_PASSWORD", SECRET)
            .env("MINIO_BROWSER", "off")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let endpoint = format!("http://127.0.0.1:{port}");
        let m = Self { child, _data: data, endpoint };
        let live = format!("{}/minio/health/live", m.endpoint);
        for _ in 0..100 {
            if reqwest::get(&live).await.is_ok_and(|r| r.status().is_success()) {
                return Some(m);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("MinIO did not start");
    }

    /// Makes bucket `name`.
    pub async fn bucket(&self, name: &str) {
        let endpoint = url::Url::parse(&self.endpoint).unwrap();
        let bucket =
            rusty_s3::Bucket::new(endpoint, rusty_s3::UrlStyle::Path, name.to_owned(), "us-east-1")
                .unwrap();
        let creds = rusty_s3::Credentials::new(USER, SECRET);
        let url = bucket.create_bucket(&creds).sign(Duration::from_secs(60));
        let r = reqwest::Client::new().put(url).send().await.unwrap();
        assert!(r.status().is_success(), "{}", r.status());
    }
}
