// SPDX-License-Identifier: AGPL-3.0-or-later

//! The flags that turn SFTP on, on `serve` for the control plane's own relay role and on `relay
//! run` for a relay of its own (§3 of docs/18-operations.md). Off unless an address is given.

use korpis_core::relay::RelayUrl;
use korpis_core::relay::sftp::SftpAddress;
use korpis_server::sftp::host_key::{fingerprint, host_key};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Debug, clap::Args)]
pub struct SftpArgs {
    /// Serve SFTP on this address as well, for the people whose SSH keys are registered (§3.1
    /// of docs/02-architecture.md). Off unless given; it extends this relay the trust §9.9 of
    /// docs/08-identity.md states.
    #[arg(
        id = "sftp-listen",
        long = "sftp-listen",
        env = "KORPIS_SFTP_LISTEN",
        value_name = "ADDRESS:PORT"
    )]
    listen: Option<SocketAddr>,
    /// The SSH host key, an Ed25519 key in OpenSSH's format, made there at the first start where
    /// the file is absent; `sftp_host_key` in the server's directory by default. Relays behind
    /// one name are given the same file.
    #[arg(
        id = "sftp-host-key",
        long = "sftp-host-key",
        env = "KORPIS_SFTP_HOST_KEY",
        value_name = "PATH"
    )]
    host_key: Option<PathBuf>,
    /// Where clients reach the SFTP, `host:port`, where that is not this server's own host and
    /// the port it listens on: behind a load balancer or a port forward. Clients are told it.
    #[arg(
        id = "sftp-advertise",
        long = "sftp-advertise",
        env = "KORPIS_SFTP_ADVERTISE",
        value_name = "HOST:PORT",
        value_parser = advertised
    )]
    advertise: Option<SftpAddress>,
}

fn advertised(s: &str) -> Result<SftpAddress, String> {
    SftpAddress::parse(s).map_err(|_| format!("{s}: where clients reach it, host:port"))
}

/// Where SFTP is served, and with what key.
pub type Bound = (TcpListener, russh::keys::PrivateKey);

impl SftpArgs {
    /// Whether SFTP is turned on.
    pub const fn on(&self) -> bool {
        self.listen.is_some()
    }

    /// Where clients are told to reach it, if that was given.
    pub fn advertised(&self) -> Option<SftpAddress> {
        self.advertise.clone()
    }

    /// Where clients are told to reach the control plane's SFTP, bound to `port`: as given, or
    /// at the host of `public_url` and that port.
    pub fn advertised_at(
        &self,
        public_url: Option<&str>,
        port: u16,
    ) -> Result<SftpAddress, String> {
        if let Some(given) = &self.advertise {
            return Ok(given.clone());
        }
        let url = public_url.ok_or("--sftp-listen needs --sftp-advertise or --public-url, so clients are told where to connect")?;
        let at = RelayUrl::parse(url.trim_end_matches('/')).map_err(|_| {
            format!("{url}: --public-url names no host to advertise SFTP at; give --sftp-advertise")
        })?;
        Ok(SftpAddress::at(&at, port))
    }

    /// The listener and the host key, kept in `dir` unless a path was given; none where SFTP is
    /// off.
    pub async fn bound(&self, dir: &Path) -> Result<Option<Bound>, Box<dyn std::error::Error>> {
        let Some(listen) = self.listen else { return Ok(None) };
        let path = self.host_key.clone().unwrap_or_else(|| dir.join("sftp_host_key"));
        let key = host_key(&path)?;
        let listener = TcpListener::bind(listen).await?;
        let addr = listener.local_addr()?;
        tracing::info!(%addr, host_key = %fingerprint(&key), "serving sftp");
        Ok(Some((listener, key)))
    }
}

/// Waits for the SFTP server, if there is one, to end, which it does only on failure, and says
/// why; forever where there is none.
pub async fn ended(sftp: Option<JoinHandle<std::io::Result<()>>>) -> String {
    match sftp {
        Some(task) => match task.await {
            Ok(Ok(())) => "the sftp server ended".to_owned(),
            Ok(Err(e)) => format!("the sftp server: {e}"),
            Err(e) => format!("the sftp server: {e}"),
        },
        None => std::future::pending().await,
    }
}
