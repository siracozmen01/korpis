// SPDX-License-Identifier: AGPL-3.0-or-later

//! The CLI's session (§9.4 of docs/08-identity.md, §7 of docs/13-surface-cli.md): signing in by
//! the device authorization grant, and the refresh credential exchanged for the access tokens a
//! command presents, rotated on every exchange and written back where it is kept.

use crate::connect::{Connection, Endpoint};
use crate::error::CliError;
use crate::vault::{Kept, Record, Vault};
use korpis_proto::korpis::v1 as pb;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// What a poll refused for being too soon adds to the interval (RFC 8628 §3.5).
const SLOW_DOWN: Duration = Duration::from_secs(5);
/// How long before it expires an access token is replaced rather than presented.
pub const MARGIN: Duration = Duration::from_secs(30);

/// Where a session's refresh credential is held.
#[derive(Debug, Clone)]
pub enum Held {
    /// Kept between commands, in the keyring or a file.
    Kept(Arc<Vault>),
    /// In this process's memory alone, for one command (`korpis login --no-store`).
    Memory(Arc<Mutex<Record>>),
}

/// An access token and when it expires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    /// The bearer.
    pub token: String,
    /// When it expires, in seconds since the epoch.
    pub expires_at: u64,
}

impl Access {
    /// How long it may still be presented: until [`MARGIN`] before it expires.
    #[must_use]
    pub fn usable_for(&self) -> Duration {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        Duration::from_secs(self.expires_at.saturating_sub(now)).saturating_sub(MARGIN)
    }
}

fn sessions(c: &Connection) -> pb::SessionServiceClient<connectrpc::client::HttpClient> {
    pb::SessionServiceClient::new(c.http(), c.config())
}

fn reason(e: &CliError) -> Option<&str> {
    match e {
        CliError::Api { reason, .. } => Some(reason),
        _ => None,
    }
}

fn say(e: &std::io::Error) -> CliError {
    CliError::Local(e.to_string())
}

/// Signs in through `c`, anonymous, by the device authorization grant, telling `out` where the
/// user approves it; the session, its refresh credential not yet exchanged.
pub async fn device_flow(c: &Connection, out: &mut impl Write) -> Result<Record, CliError> {
    let begun = sessions(c).authorize_device(pb::AuthorizeDeviceRequest::default()).await?;
    let a = begun.into_owned();
    writeln!(out, "To sign in, open {} and enter the code {}", a.verification_uri, a.user_code)
        .and_then(|()| writeln!(out, "or open {}", a.verification_uri_complete))
        .and_then(|()| writeln!(out, "Waiting for the code to be approved..."))
        .map_err(|e| say(&e))?;
    let mut interval = Duration::from_secs(u64::from(a.interval.max(1)));
    let request = pb::DeviceTokenRequest { device_code: a.device_code, ..Default::default() };
    loop {
        tokio::time::sleep(interval).await;
        match sessions(c).device_token(request.clone()).await.map_err(CliError::from) {
            Ok(signed_in) => {
                let refresh = signed_in.into_owned().refresh;
                return Ok(Record { refresh, access: String::new(), access_expires_at: 0 });
            }
            Err(e) if reason(&e) == Some("device.pending") => {}
            Err(e) if reason(&e) == Some("rate.limited") => interval += SLOW_DOWN,
            Err(e) => return Err(e),
        }
    }
}

/// What `kept` says of where the session went, for the user.
#[must_use]
pub fn kept(kept: &Kept) -> String {
    match kept {
        Kept::Keyring => "signed in; the session is kept in the system keyring".into(),
        Kept::File(path, why) => format!(
            "signed in; no keyring was reachable ({why}), so the session is kept in {}, readable \
             by you alone",
            path.display()
        ),
    }
}

/// An access token for the session `held` holds: the one last exchanged for while it is still
/// usable, and otherwise a new one, exchanged through `c`, anonymous, with the credential that
/// replaces the old kept. A kept session is exchanged under its lock, so commands run at once take
/// turns rather than present one credential twice, which would end the session.
pub async fn access(c: &Connection, held: &Held) -> Result<Access, CliError> {
    match held {
        Held::Kept(vault) => {
            let _lock = vault.lock().await?;
            let record = vault.load().await?.ok_or_else(signed_out)?;
            match refreshed(c, &record).await {
                Ok(Some(next)) => {
                    vault.save(&next).await?;
                    Ok(access_of(&next))
                }
                Ok(None) => Ok(access_of(&record)),
                Err(e) => {
                    if ended(&e) {
                        vault.forget().await?;
                    }
                    Err(e)
                }
            }
        }
        Held::Memory(record) => {
            let mut record = record.lock().await;
            if let Some(next) = refreshed(c, &record).await? {
                *record = next;
            }
            Ok(access_of(&record))
        }
    }
}

fn access_of(r: &Record) -> Access {
    Access { token: r.access.clone(), expires_at: r.access_expires_at }
}

/// `record` exchanged for a new access token, unless its own is still usable.
async fn refreshed(c: &Connection, record: &Record) -> Result<Option<Record>, CliError> {
    if !record.access.is_empty() && access_of(record).usable_for() > Duration::ZERO {
        return Ok(None);
    }
    let request = pb::ExchangeRequest { refresh: record.refresh.clone(), ..Default::default() };
    let e = sessions(c).exchange(request).await.map_err(|e| match CliError::from(e) {
        CliError::Api { code: pb::ErrorCode::ERROR_CODE_UNAUTHENTICATED, .. } => {
            CliError::Local(ENDED.into())
        }
        other => other,
    })?;
    let e = e.into_owned();
    let expires_at =
        e.access_expires_at.as_option().map_or(0, |t| u64::try_from(t.seconds).unwrap_or(0));
    Ok(Some(Record { refresh: e.refresh, access: e.access_token, access_expires_at: expires_at }))
}

/// What a session that has ended is told.
const ENDED: &str = "the session has ended: run `korpis login` again";

fn ended(e: &CliError) -> bool {
    matches!(e, CliError::Local(m) if m == ENDED)
}

fn signed_out() -> CliError {
    CliError::Local("not signed in: run `korpis login`".into())
}

/// Ends the session `held` holds through `c`, anonymous, and forgets a kept one.
pub async fn sign_out(c: &Connection, held: &Held) -> Result<(), CliError> {
    let refresh = match held {
        Held::Kept(vault) => {
            let _lock = vault.lock().await?;
            let record = vault.load().await?;
            vault.forget().await?;
            record.map(|r| r.refresh)
        }
        Held::Memory(record) => Some(record.lock().await.refresh.clone()),
    };
    let request =
        pb::SignOutRequest { refresh: refresh.ok_or_else(signed_out)?, ..Default::default() };
    sessions(c).sign_out(request).await?;
    Ok(())
}

/// The connection to `server`, trusting `ca`, a command is borne by: `token` when one is given,
/// for automation, which takes precedence; otherwise `held`, or the session kept for `server`,
/// if there is one; otherwise none, for the calls anyone may make.
pub async fn connection(
    server: &str,
    ca: PathBuf,
    token: Option<String>,
    held: Option<Held>,
) -> Result<Connection, CliError> {
    let token = token.filter(|t| !t.is_empty());
    let given = token.is_some();
    let c = Connection::open(&Endpoint {
        server: server.to_owned(),
        ca,
        token: token.unwrap_or_default(),
    })?;
    if given {
        return Ok(c);
    }
    if let Some(held) = held {
        return c.with_session(held).await;
    }
    let vault = Vault::new(server, Vault::default_dir()?).await;
    if vault.load().await?.is_none() {
        return Ok(c);
    }
    c.with_session(Held::Kept(Arc::new(vault))).await
}
