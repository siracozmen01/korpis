// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pebble, with an authority made for it, and `pebble-challtestsrv`'s DNS server answering every
//! name with one address, so a test's validations reach the machine the test runs on.

use crate::acme::AcmeConfig;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Why a server could not be run.
#[derive(Debug, thiserror::Error)]
pub enum TestingError {
    /// It is not installed: scripts/test-tools.sh installs it.
    #[error("{0} is not installed (scripts/test-tools.sh)")]
    NotInstalled(PathBuf),
    /// Starting it, or writing what it reads, failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// Its certificate could not be made.
    #[error("its certificate: {0}")]
    Certificate(#[from] rcgen::Error),
    /// It did not listen in time.
    #[error("{0} did not listen")]
    NotListening(&'static str),
}

fn tool(name: &str) -> Result<PathBuf, TestingError> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tools/bin").join(name);
    if path.exists() { Ok(path) } else { Err(TestingError::NotInstalled(path)) }
}

fn free_port() -> Result<u16, TestingError> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Answers once something listens on `port` of loopback, within ten seconds.
async fn listening(port: u16, what: &'static str) -> Result<(), TestingError> {
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(TestingError::NotListening(what))
}

/// A child process, killed when dropped.
#[derive(Debug)]
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// How Pebble validates.
#[derive(Debug, Clone, Copy, Default)]
pub struct PebbleOptions {
    /// Take every challenge as valid, validating nothing.
    pub always_valid: bool,
    /// The port HTTP-01 validations connect to; Pebble's own default when none.
    pub http_port: Option<u16>,
    /// The port TLS-ALPN-01 validations connect to; one nothing listens on when none.
    pub tls_port: Option<u16>,
    /// The DNS server validations resolve names through; the system's when none.
    pub dns: Option<SocketAddr>,
}

/// A running Pebble.
#[derive(Debug)]
pub struct Pebble {
    _running: Running,
    _dir: tempfile::TempDir,
    /// Its directory's URL.
    pub directory: String,
    /// Its management interface, where its issuing roots are: `<management>/roots/0`.
    pub management: String,
    /// The authority its own certificate, and its management interface's, are trusted by.
    pub ca: PathBuf,
}

impl Pebble {
    /// Pebble, validating as `options` say.
    ///
    /// # Errors
    /// It is not installed, or did not start.
    pub async fn start(options: PebbleOptions) -> Result<Self, TestingError> {
        let bin = tool("pebble")?;
        let dir = tempfile::tempdir()?;
        let made =
            rcgen::generate_simple_self_signed(vec!["127.0.0.1".into(), "localhost".into()])?;
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        std::fs::write(&cert, made.cert.pem())?;
        std::fs::write(&key, made.signing_key.serialize_pem())?;
        let (listen, manage) = (free_port()?, free_port()?);
        let config = serde_json::json!({"pebble": {
            "listenAddress": format!("127.0.0.1:{listen}"),
            "managementListenAddress": format!("127.0.0.1:{manage}"),
            "certificate": cert, "privateKey": key,
            "httpPort": options.http_port.unwrap_or(5002), "tlsPort": options.tls_port.map_or_else(free_port, Ok)?,
            "ocspResponderURL": "", "externalAccountBindingRequired": false,
            "retryAfter": {"authz": 1, "order": 1}, "keyAlgorithm": "ecdsa",
            "profiles": {"default": {"description": "test", "validityPeriod": 7_776_000}},
        }});
        let path = dir.path().join("pebble.json");
        std::fs::write(&path, config.to_string())?;
        let mut command = Command::new(bin);
        command.arg("-config").arg(&path);
        if let Some(dns) = options.dns {
            command.arg("-dnsserver").arg(dns.to_string());
        }
        let child = command
            .env("PEBBLE_VA_ALWAYS_VALID", if options.always_valid { "1" } else { "0" })
            .env("PEBBLE_VA_NOSLEEP", "1")
            .env("PEBBLE_WFE_NONCEREJECT", "0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let pebble = Self {
            _running: Running(child),
            _dir: dir,
            directory: format!("https://127.0.0.1:{listen}/dir"),
            management: format!("https://127.0.0.1:{manage}"),
            ca: cert,
        };
        listening(listen, "pebble").await?;
        Ok(pebble)
    }

    /// The control plane's settings for this directory, its terms agreed.
    #[must_use]
    pub fn config(&self) -> AcmeConfig {
        AcmeConfig {
            directory: self.directory.clone(),
            ca: Some(self.ca.clone()),
            contact: vec!["mailto:operator@example.com".into()],
            agreed: true,
        }
    }
}

/// `pebble-challtestsrv`'s DNS server, answering every name's `A` query with one address and
/// serving nothing else.
#[derive(Debug)]
pub struct ChallengeDns {
    _running: Running,
    /// Where it answers.
    pub address: SocketAddr,
}

impl ChallengeDns {
    /// The server, answering every name with `ipv4`.
    ///
    /// # Errors
    /// It is not installed, or did not start.
    pub async fn start(ipv4: Ipv4Addr) -> Result<Self, TestingError> {
        let bin = tool("pebble-challtestsrv")?;
        let (dns, manage) = (free_port()?, free_port()?);
        let child = Command::new(bin)
            .args(["-http01", "", "-https01", "", "-tlsalpn01", "", "-doh", ""])
            .arg("-dnsserver")
            .arg(format!("127.0.0.1:{dns}"))
            .arg("-management")
            .arg(format!("127.0.0.1:{manage}"))
            .arg("-defaultIPv4")
            .arg(ipv4.to_string())
            .arg("-defaultIPv6")
            .arg("")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let running = Running(child);
        listening(manage, "pebble-challtestsrv").await?;
        Ok(Self { _running: running, address: SocketAddr::from(([127, 0, 0, 1], dns)) })
    }
}
