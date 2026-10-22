// SPDX-License-Identifier: AGPL-3.0-or-later

//! The file session as the relay holds one for an SFTP client: each request one `FileRequest`
//! data frame towards the node and each answer the `FileResponse` of the same number, in order,
//! requests sent ahead of their answers as the CLI sends them (§3.2 of docs/06-storage.md). The
//! token is renewed before it expires for as long as the SFTP connection lasts, judged here as
//! the relay judges a client's renewal, so a session the relay itself is the client of is held to
//! what any other is.

use super::{Authorizer, Granted};
use crate::api::Carried;
use crate::relay::Verifier;
use crate::standalone::feed::FilesAsked;
use buffa::Message as _;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use korpis_proto::korpis::v1::__buffa::oneof::relay_response::Message as Reply;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use std::time::{Duration, SystemTime};

/// How long before its token expires a session renews it.
const RENEW_BEFORE: Duration = Duration::from_secs(30);

/// Why a session is over: it ended, or could not go on.
pub(super) type Lost = String;

/// What the node answered a request: what it did, or why it did not.
pub(super) type Answered = Result<Response, pb::FileError>;

/// An open file session.
pub(super) struct FileSession<V> {
    carried: Carried<V>,
    granted: Granted,
    /// What the token was asked for with, to ask its renewals with.
    asked: FilesAsked,
    sent: u64,
    answered: u64,
}

fn frame(frame: Frame) -> agent::CarryDown {
    let frame = pb::SessionFrame { frame: Some(frame), ..Default::default() };
    agent::CarryDown { frame: frame.into(), ..Default::default() }
}

impl<V: Verifier> FileSession<V> {
    /// The session `carried`, opened with `granted`, which was asked for as `asked` says.
    pub(super) const fn new(carried: Carried<V>, granted: Granted, asked: FilesAsked) -> Self {
        Self { carried, granted, asked, sent: 0, answered: 0 }
    }

    /// How long until the token should be renewed.
    pub(super) fn renew_in(&self) -> Duration {
        let at = self.granted.expires_at.checked_sub(RENEW_BEFORE);
        let at = at.unwrap_or(SystemTime::UNIX_EPOCH);
        at.duration_since(SystemTime::now()).unwrap_or_default()
    }

    /// Sends `request`, ahead of the answers to those before it.
    pub(super) async fn send(&mut self, request: Request) -> Result<(), Lost> {
        self.sent += 1;
        let r = pb::FileRequest { id: self.sent, request: Some(request), ..Default::default() };
        let sent = self.carried.to_node.send(frame(Frame::Data(r.encode_to_vec()))).await;
        sent.map_err(|_| "the node ended the session".to_owned())
    }

    /// What the node sends next; cancelling it loses nothing.
    pub(super) async fn heard(&mut self) -> Option<pb::RelayResponse> {
        self.carried.from_node.recv().await
    }

    /// The answer `heard` holds, if it holds one: the next request's, in order. The session's
    /// opening and a renewal's acknowledgement hold none.
    pub(super) fn answer(
        &mut self,
        heard: Option<pb::RelayResponse>,
    ) -> Result<Option<Answered>, Lost> {
        let heard = heard.ok_or_else(|| "the session ended".to_owned())?;
        let Some(Reply::Frame(f)) = heard.message else { return Ok(None) };
        match f.frame {
            Some(Frame::Data(bytes)) => {
                let answer = pb::FileResponse::decode_from_slice(&bytes)
                    .map_err(|e| format!("an answer the node sent: {e}"))?;
                self.answered += 1;
                if answer.id != self.answered {
                    return Err("an answer out of order".to_owned());
                }
                match answer.response {
                    Some(Response::Error(e)) => Ok(Some(Err(*e))),
                    Some(r) => Ok(Some(Ok(r))),
                    None => Err("an empty answer".to_owned()),
                }
            }
            Some(Frame::End(end)) => {
                Err(format!("the session ended: {}: {}", end.code, end.message))
            }
            _ => Ok(None),
        }
    }

    /// Asks `request` and waits for its answer, renewing the token while it waits. Only with
    /// nothing else ahead of it.
    pub(super) async fn ask<A: Authorizer>(
        &mut self,
        request: Request,
        authorizer: &A,
    ) -> Result<Answered, Lost> {
        self.send(request).await?;
        loop {
            let renew_in = self.renew_in();
            tokio::select! {
                heard = self.heard() => {
                    if let Some(answer) = self.answer(heard)? {
                        return Ok(answer);
                    }
                }
                () = tokio::time::sleep(renew_in) => self.renew(authorizer).await?,
            }
        }
    }

    /// Renews the token: asked for again, judged as the relay judges a renewal, and sent.
    pub(super) async fn renew<A: Authorizer>(&mut self, authorizer: &A) -> Result<(), Lost> {
        let asked = FilesAsked { renews: Some(self.granted.token.clone()), ..self.asked.clone() };
        let renewed = authorizer.authorize(asked).await?;
        self.carried.judge.judge(&renewed.token)?;
        let sent = self.carried.to_node.send(frame(Frame::Renew(renewed.token.clone()))).await;
        sent.map_err(|_| "the node ended the session".to_owned())?;
        self.granted = renewed;
        Ok(())
    }

    /// Ends the session: the client is done.
    pub(super) async fn end(self) {
        let end = pb::SessionEnd::default();
        let _ = self.carried.to_node.send(frame(Frame::End(Box::new(end)))).await;
    }
}
