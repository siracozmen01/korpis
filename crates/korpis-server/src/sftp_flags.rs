// SPDX-License-Identifier: AGPL-3.0-or-later

//! The flags that turn SFTP on, on `serve` for the control plane's own relay role and on `relay
//! run` for a relay of its own (§3 of docs/18-operations.md). Off unless an address is given.

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
    #[arg(long, env = "KORPIS_SFTP_LISTEN")]
    sftp_listen: Option<SocketAddr>,
    /// The SSH host key, an Ed25519 key in OpenSSH's format, made there at the first start where
    /// the file is absent; `sftp_host_key` in the server's directory by default. Relays behind
    /// one name are given the same file.
    #[arg(long, env = "KORPIS_SFTP_HOST_KEY")]
    sftp_host_key: Option<PathBuf>,
}

/// Where SFTP is served, and with what key.
pub type Bound = (TcpListener, russh::keys::PrivateKey);

impl SftpArgs {
    /// Whether SFTP is turned on.
    pub const fn on(&self) -> bool {
        self.sftp_listen.is_some()
    }

    /// The listener and the host key, kept in `dir` unless a path was given; none where SFTP is
    /// off.
    pub async fn bound(&self, dir: &Path) -> Result<Option<Bound>, Box<dyn std::error::Error>> {
        let Some(listen) = self.sftp_listen else { return Ok(None) };
        let path = self.sftp_host_key.clone().unwrap_or_else(|| dir.join("sftp_host_key"));
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
