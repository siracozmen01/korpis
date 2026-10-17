// SPDX-License-Identifier: AGPL-3.0-or-later

//! A real S3-compatible store for tests: MinIO run as a plain host process on a free loopback
//! port, its data in a directory that goes when it does. `scripts/test-tools.sh` installs it into
//! the workspace's `.tools`; where it is not installed, starting one answers `None` and the test
//! says it did not run. Only with the `testing` feature.

use rusty_s3::actions::S3Action as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The root user tests sign as.
pub const USER: &str = "korpis-test";
/// Its secret.
pub const SECRET: &str = "korpis-test-secret";

/// A running MinIO, stopped when dropped.
#[derive(Debug)]
pub struct Minio {
    child: Child,
    data: PathBuf,
    /// Where it listens, `http://127.0.0.1:<port>`.
    pub endpoint: String,
}

impl Drop for Minio {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.tools/bin/minio")
}

fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn failed(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Minio {
    /// A MinIO serving with its data beneath `scratch`, or `None` where none is installed.
    ///
    /// A free port is free only until another test takes it, and a MinIO that lost its port to
    /// another exits while the other answers on it, readily; so each start proves the server it
    /// reached is its own, by making a bucket and finding it in its own data, and starts again on
    /// other ports if it is not.
    pub async fn start(scratch: &std::path::Path) -> Result<Option<Self>, String> {
        if !binary().exists() {
            tracing::warn!("MinIO is not installed (scripts/test-tools.sh): the test does not run");
            return Ok(None);
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        for _ in 0..5 {
            let m = Self::spawn(scratch)?;
            if m.ready().await && m.owns().await? {
                return Ok(Some(m));
            }
        }
        Err("MinIO did not start on a port of its own".into())
    }

    fn spawn(scratch: &std::path::Path) -> Result<Self, String> {
        let data = scratch.join(format!("minio-{}", free_port().map_err(failed)?));
        std::fs::create_dir_all(&data).map_err(failed)?;
        let (port, console) = (free_port().map_err(failed)?, free_port().map_err(failed)?);
        let child = Command::new(binary())
            .args(["server", "--quiet", "--address", &format!("127.0.0.1:{port}")])
            .args(["--console-address", &format!("127.0.0.1:{console}")])
            .arg(&data)
            .env("MINIO_ROOT_USER", USER)
            .env("MINIO_ROOT_PASSWORD", SECRET)
            .env("MINIO_BROWSER", "off")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(failed)?;
        Ok(Self { child, data, endpoint: format!("http://127.0.0.1:{port}") })
    }

    /// Whether something answers ready at its endpoint: ready, not merely live, since a live
    /// server still answers 503 until it is initialized.
    async fn ready(&self) -> bool {
        let ready = format!("{}/minio/health/ready", self.endpoint);
        for _ in 0..200 {
            if reqwest::get(&ready).await.is_ok_and(|r| r.status().is_success()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    /// Whether the server at its endpoint is this one: a bucket made there is in its data.
    async fn owns(&self) -> Result<bool, String> {
        let nanos = std::time::SystemTime::UNIX_EPOCH.elapsed().map_or(0, |d| d.as_nanos());
        let probe = format!("probe-{}-{nanos}", std::process::id());
        if self.bucket(&probe).await.is_err() {
            return Ok(false);
        }
        let ours = self.data.join(&probe).is_dir();
        if ours {
            let endpoint = url::Url::parse(&self.endpoint).map_err(failed)?;
            let bucket =
                rusty_s3::Bucket::new(endpoint, rusty_s3::UrlStyle::Path, probe, "us-east-1")
                    .map_err(failed)?;
            let creds = rusty_s3::Credentials::new(USER, SECRET);
            let url = bucket.delete_bucket(&creds).sign(Duration::from_secs(60));
            // A server that made the probe and then stopped answering is not one to test
            // against: another is started, as for one that is not ours.
            if reqwest::Client::new().delete(url).send().await.is_err() {
                return Ok(false);
            }
        }
        Ok(ours)
    }

    /// Makes bucket `name`.
    pub async fn bucket(&self, name: &str) -> Result<(), String> {
        let endpoint = url::Url::parse(&self.endpoint).map_err(failed)?;
        let bucket =
            rusty_s3::Bucket::new(endpoint, rusty_s3::UrlStyle::Path, name.to_owned(), "us-east-1")
                .map_err(failed)?;
        let creds = rusty_s3::Credentials::new(USER, SECRET);
        let mut status = reqwest::StatusCode::SERVICE_UNAVAILABLE;
        // A server that says it is ready may still refuse briefly while its drives settle.
        for _ in 0..50 {
            let url = bucket.create_bucket(&creds).sign(Duration::from_secs(60));
            status = reqwest::Client::new().put(url).send().await.map_err(failed)?.status();
            if status != reqwest::StatusCode::SERVICE_UNAVAILABLE {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if status.is_success() { Ok(()) } else { Err(format!("creating {name}: {status}")) }
    }

    /// Access to `prefix` in `bucket` as the root user.
    #[must_use]
    pub fn access(&self, bucket: &str, prefix: &str) -> crate::s3::S3Config {
        crate::s3::S3Config {
            endpoint: self.endpoint.clone(),
            region: "us-east-1".into(),
            bucket: bucket.into(),
            key_prefix: prefix.into(),
            access_key_id: USER.into(),
            secret_access_key: SECRET.into(),
            session_token: String::new(),
            path_style: true,
            certificate: Vec::new(),
        }
    }
}
