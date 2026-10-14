// SPDX-License-Identifier: AGPL-3.0-or-later

//! A data-path session with one workload, as the interactive commands open one (§3.1 of
//! docs/02-architecture.md): a capability token minted for the action on the one workload, with
//! the route the gateway seals with it, presented at a relay, and renewed on the session itself
//! before it expires.

use crate::connect::Connection;
use crate::error::CliError;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::relay_request::Message as Request;
use korpis_proto::korpis::v1::__buffa::oneof::relay_response::Message as Reply;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long before its token expires a session renews it.
pub const RENEW_BEFORE: Duration = Duration::from_secs(30);

/// An open session.
pub type Session = connectrpc::client::BidiStream<
    <connectrpc::client::HttpClient as connectrpc::client::ClientTransport>::ResponseBody,
    pb::RelayRequest,
    pb::__buffa::view::RelayResponseView<'static>,
>;

/// A token for one action on one workload, with the route it came with.
#[derive(Debug, Clone)]
pub struct Minted {
    /// The action it carries.
    pub action: &'static str,
    /// The token.
    pub token: String,
    /// The node, sealed.
    pub route: String,
    /// When it stops carrying the session unless renewed.
    pub expires_at: SystemTime,
}

impl Minted {
    /// When the session should renew it.
    #[must_use]
    pub fn renew_in(&self) -> Duration {
        let at = self.expires_at.checked_sub(RENEW_BEFORE).unwrap_or(UNIX_EPOCH);
        at.duration_since(SystemTime::now()).unwrap_or_default()
    }
}

/// A token for `action` on `workload`, and its route.
pub async fn mint(
    c: &Connection,
    workload: &str,
    action: &'static str,
) -> Result<Minted, CliError> {
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload)
        .map_err(|e| CliError::Local(e.to_string()))?;
    let on = korpis_core::authz::ObjectRef::new(korpis_core::authz::Kind::Workload, id.uuid());
    let request = pb::MintTokenRequest {
        actions: vec![action.to_owned()],
        scope: korpis_wire::scope::to_pb(&korpis_core::scope::Scope::Object(on)).into(),
        ..Default::default()
    };
    let minted = pb::GrantServiceClient::new(c.http(), c.config()).mint_token(request).await?;
    let minted = minted.into_owned();
    if minted.route.is_empty() {
        return Err(CliError::Local(format!("{workload} is not placed on a node: no console")));
    }
    let at = minted.expires_at.as_option().map_or(0, |t| t.seconds);
    let expires_at = UNIX_EPOCH + Duration::from_secs(u64::try_from(at).unwrap_or(0));
    Ok(Minted { action, token: minted.token, route: minted.route, expires_at })
}

/// Where sessions are opened: the relay named, the control plane if it serves the role, or the
/// first relay it names.
pub async fn relay(c: &Connection, named: Option<&str>) -> Result<Connection, CliError> {
    if let Some(url) = named {
        return c.at(url);
    }
    let info = pb::ServerInfoServiceClient::new(c.http(), c.config())
        .get_server_info(pb::GetServerInfoRequest::default())
        .await?
        .into_owned();
    if info.control_plane_relay {
        return Ok(c.clone());
    }
    let url = info.relays.first().ok_or_else(|| {
        CliError::Local("no relay serves sessions: the control plane names none".into())
    })?;
    c.at(url)
}

/// A request carrying `message`.
#[must_use]
pub fn request(message: Request) -> pb::RelayRequest {
    pb::RelayRequest { message: Some(message), ..Default::default() }
}

/// A request carrying `frame`.
#[must_use]
pub fn frame(frame: Frame) -> pb::RelayRequest {
    request(Request::Frame(Box::new(pb::SessionFrame { frame: Some(frame), ..Default::default() })))
}

/// A session opened at `at` for `workload` with `minted`, its params `params`.
pub async fn open(
    at: &Connection,
    workload: &str,
    minted: &Minted,
    params: Vec<u8>,
) -> Result<Session, CliError> {
    let mut session = pb::RelayServiceClient::new(at.http(), at.config()).open().await?;
    let open = pb::RelayOpen {
        token: minted.token.clone(),
        action: minted.action.to_owned(),
        workload: workload.to_owned(),
        route: minted.route.clone(),
        params,
        ..Default::default()
    };
    session.send(request(Request::Open(Box::new(open)))).await?;
    match session.message::<pb::RelayResponse>().await?.map(|m| m.to_owned_message().message) {
        Some(Some(Reply::Opened(_))) => Ok(session),
        _ => Err(CliError::Transport("the relay answered the open with no session".into())),
    }
}

/// Renews `minted` on `session` with a fresh token for the same action.
pub async fn renew(
    c: &Connection,
    workload: &str,
    session: &mut Session,
    minted: &mut Minted,
) -> Result<(), CliError> {
    *minted = mint(c, workload, minted.action).await?;
    session.send(frame(Frame::Renew(minted.token.clone()))).await?;
    Ok(())
}
