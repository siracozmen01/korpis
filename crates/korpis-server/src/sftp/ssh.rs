// SPDX-License-Identifier: AGPL-3.0-or-later

//! The SSH half: a connection authenticates by public key alone, as the workload its user names,
//! and opens one session channel whose one request may be the `sftp` subsystem. The token is
//! asked for only once the client has signed with the key, so a key offered without its
//! signature tells nothing about whether it is registered; every refusal is the same refusal,
//! after the same delay.

use super::files::Files;
use super::node::FileSession;
use super::{Authorizer, Granted};
use crate::api::RelayRole;
use crate::relay::Verifier;
use crate::standalone::feed::FilesAsked;
use ipnet::IpNet;
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::v1 as pb;
use russh::keys::ssh_key::{HashAlg, PublicKey};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Config, Handler, Msg, Session};
use russh::{Channel, ChannelId, ChannelOpenFailure, MethodKind, MethodSet, Preferred};
use std::borrow::Cow;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

/// How long a connection may be idle, its session open or not, before it is closed.
const IDLE: Duration = Duration::from_secs(600);
/// How many keys a connection may try.
const ATTEMPTS: usize = 6;

/// The host and client key algorithms offered: RSA only with SHA-2 (RFC 8332), never SHA-1.
const KEYS: &[Algorithm] = &[
    Algorithm::Ed25519,
    Algorithm::Ecdsa { curve: russh::keys::EcdsaCurve::NistP256 },
    Algorithm::Ecdsa { curve: russh::keys::EcdsaCurve::NistP384 },
    Algorithm::Rsa { hash: Some(HashAlg::Sha512) },
    Algorithm::Rsa { hash: Some(HashAlg::Sha256) },
];

fn config(host_key: PrivateKey) -> Config {
    let methods: &[MethodKind] = &[MethodKind::PublicKey];
    Config {
        server_id: russh::SshId::Standard(Cow::Borrowed("SSH-2.0-korpis")),
        methods: MethodSet::from(methods),
        keys: vec![host_key],
        preferred: Preferred { key: Cow::Borrowed(KEYS), ..Preferred::DEFAULT },
        max_auth_attempts: ATTEMPTS,
        inactivity_timeout: Some(IDLE),
        keepalive_interval: Some(Duration::from_secs(30)),
        nodelay: true,
        ..Config::default()
    }
}

/// Serves SFTP on `listener` with `host_key` for `role`, asking `authorizer` for each client's
/// token, and reading a PROXY header from every connection whose peer is in `proxies`, as the
/// relay's API listener does. Ends only if accepting fails.
pub async fn serve<V: Verifier, A: Authorizer>(
    listener: TcpListener,
    host_key: PrivateKey,
    proxies: Arc<[IpNet]>,
    (role, authorizer): (Arc<RelayRole<V>>, Arc<A>),
) -> std::io::Result<()> {
    let config = Arc::new(config(host_key));
    loop {
        let (mut stream, peer) = listener.accept().await?;
        let (config, proxies) = (Arc::clone(&config), Arc::clone(&proxies));
        let (role, authorizer) = (Arc::clone(&role), Arc::clone(&authorizer));
        tokio::spawn(async move {
            let client = match crate::accept::client(&mut stream, peer, &proxies).await {
                Ok(client) => client.ip(),
                Err(e) => return tracing::debug!(%peer, error = %e, "sftp connection refused"),
            };
            let handler = Client { ip: client, role, authorizer, granted: None, channel: None };
            let ended = match russh::server::run_stream(config, stream, handler).await {
                Ok(running) => running.await,
                Err(e) => Err(e),
            };
            if let Err(e) = ended {
                tracing::debug!(%client, error = %e, "sftp connection ended");
            }
        });
    }
}

/// One SSH connection.
struct Client<V, A> {
    ip: IpAddr,
    role: Arc<RelayRole<V>>,
    authorizer: Arc<A>,
    /// What it was authenticated as, and the token that gave it.
    granted: Option<(FilesAsked, Granted)>,
    /// Its session channel, until the subsystem starts on it.
    channel: Option<Channel<Msg>>,
}

impl<V: Verifier, A: Authorizer> Client<V, A> {
    /// Opens the file session the token was granted for, at this relay, from the client.
    async fn open(&self) -> Result<FileSession<V>, String> {
        let (asked, granted) = self.granted.clone().ok_or("not authenticated")?;
        let open = pb::RelayOpen {
            token: granted.token.clone(),
            action: granted.action.clone(),
            workload: korpis_wire::wire_id(IdKind::Workload, asked.workload),
            route: granted.route.clone(),
            ..Default::default()
        };
        let carried = self.role.carry_from(self.ip, open, String::new()).await;
        let carried = carried.map_err(|e| e.message.unwrap_or_else(|| "refused".into()))?;
        Ok(FileSession::new(carried, granted, asked))
    }
}

impl<V: Verifier, A: Authorizer> Handler for Client<V, A> {
    type Error = russh::Error;

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let Ok(workload) = Id::parse(IdKind::Workload, user) else { return Ok(Auth::reject()) };
        let Ok(blob) = key.to_bytes() else { return Ok(Auth::reject()) };
        let asked =
            FilesAsked { key: blob, workload: workload.uuid(), client: self.ip, renews: None };
        match self.authorizer.authorize(asked.clone()).await {
            Ok(granted) => {
                self.granted = Some((asked, granted));
                Ok(Auth::Accept)
            }
            Err(e) => {
                tracing::debug!(client = %self.ip, %user, error = %e, "sftp key refused");
                Ok(Auth::reject())
            }
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel.is_some() || self.granted.is_none() {
            reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
        } else {
            self.channel = Some(channel);
            reply.accept().await;
        }
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let ours = self.channel.as_ref().is_some_and(|c| c.id() == id);
        if name != "sftp" || !ours {
            return refuse(session, id, "only the sftp subsystem is served");
        }
        let opened = self.open().await;
        let (Some(channel), Some((_, granted))) = (self.channel.take(), self.granted.as_ref())
        else {
            return refuse(session, id, "not authenticated");
        };
        match opened {
            Ok(files) => {
                session.channel_success(id)?;
                let write = granted.action == crate::sftp::files::WRITE;
                let (stream, files) = (channel.into_stream(), Files::new(files, write));
                tokio::spawn(files.serve(stream, Arc::clone(&self.authorizer)));
                Ok(())
            }
            Err(e) => refuse(session, id, &format!("the files of this workload: {e}")),
        }
    }

    fn shell_request(
        &mut self,
        id: ChannelId,
        s: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        std::future::ready(refuse(s, id, "this server serves SFTP alone"))
    }

    fn exec_request(
        &mut self,
        id: ChannelId,
        _: &[u8],
        s: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        std::future::ready(refuse(s, id, "this server serves SFTP alone"))
    }

    #[allow(clippy::too_many_arguments, reason = "the trait's signature")]
    fn pty_request(
        &mut self,
        id: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        s: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        std::future::ready(s.channel_failure(id))
    }
}

/// Refuses the request on channel `id`, saying why on its error stream, and closes it.
fn refuse(session: &mut Session, id: ChannelId, why: &str) -> Result<(), russh::Error> {
    session.extended_data(id, 1, format!("korpis: {why}\r\n").into_bytes())?;
    session.channel_failure(id)?;
    session.close(id)
}
