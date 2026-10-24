// SPDX-License-Identifier: AGPL-3.0-or-later

//! A file session as the CLI holds one (§3.2 of docs/06-storage.md): a token minted for
//! `workload.file.read` or `workload.file.write` on one workload, a session opened at a relay,
//! each request one `FileRequest` data frame and each answer the `FileResponse` frame of the same
//! number, in order. Requests may be sent ahead of their answers; the token is renewed on the
//! session while an answer is awaited, so a long transfer outlives it.

use crate::cmd::session::{Minted, Session, frame, mint, open, relay, renew};
use crate::connect::Connection;
use crate::error::CliError;
use buffa::Message as _;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use korpis_proto::korpis::v1::__buffa::oneof::relay_response::Message as Reply;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;

/// The action that reads.
pub const READ: &str = "workload.file.read";
/// The action that writes, and reads.
pub const WRITE: &str = "workload.file.write";

/// An open file session.
pub struct Remote {
    c: Connection,
    workload: String,
    session: Session,
    minted: Minted,
    sent: u64,
    answered: u64,
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote").field("workload", &self.workload).finish_non_exhaustive()
    }
}

/// A refusal of the node's, as the CLI reports the API's: its code mapped, its reason
/// `file.<code>`, and the path it concerned as the field.
fn refused(path: &str, e: &pb::FileError) -> CliError {
    use pb::FileErrorCode as F;
    let known = e.code.as_known().unwrap_or(F::FILE_ERROR_CODE_UNSPECIFIED);
    let (code, reason) = match known {
        F::FILE_ERROR_CODE_NOT_FOUND => (pb::ErrorCode::ERROR_CODE_NOT_FOUND, "not_found"),
        F::FILE_ERROR_CODE_EXISTS => (pb::ErrorCode::ERROR_CODE_CONFLICT, "exists"),
        F::FILE_ERROR_CODE_NOT_EMPTY => (pb::ErrorCode::ERROR_CODE_CONFLICT, "not_empty"),
        F::FILE_ERROR_CODE_NOT_A_DIRECTORY => {
            (pb::ErrorCode::ERROR_CODE_CONFLICT, "not_a_directory")
        }
        F::FILE_ERROR_CODE_IS_A_DIRECTORY => (pb::ErrorCode::ERROR_CODE_CONFLICT, "is_a_directory"),
        F::FILE_ERROR_CODE_FULL => (pb::ErrorCode::ERROR_CODE_QUOTA_EXCEEDED, "full"),
        F::FILE_ERROR_CODE_OUTSIDE => (pb::ErrorCode::ERROR_CODE_DENIED, "outside"),
        F::FILE_ERROR_CODE_REFUSED => (pb::ErrorCode::ERROR_CODE_DENIED, "refused"),
        _ => (pb::ErrorCode::ERROR_CODE_UNAVAILABLE, "io"),
    };
    CliError::Api {
        code,
        reason: format!("file.{reason}"),
        field: path.to_owned(),
        message: super::printable(&e.message),
    }
}

/// The path a request concerns, for its refusal.
fn path_of(r: &Request) -> &str {
    match r {
        Request::List(x) => &x.path,
        Request::Stat(x) => &x.path,
        Request::Read(x) => &x.path,
        Request::Write(x) => &x.path,
        Request::Mkdir(x) => &x.path,
        Request::Rename(x) => &x.from,
        Request::Remove(x) => &x.path,
    }
}

impl Remote {
    /// A session on `workload`, an identifier, to write when `write`, at the relay `at` or the
    /// first the control plane names.
    ///
    /// # Errors
    /// The token could not be minted, or the session opened.
    pub async fn open(
        c: &Connection,
        workload: &str,
        write: bool,
        at: Option<&str>,
    ) -> Result<Self, CliError> {
        Self::opened(c, workload, (if write { WRITE } else { READ }, Vec::new()), at).await
    }

    /// A session of `action` on `workload` that carries file requests, opened with `params`, at
    /// the relay `at` or the first the control plane names: a backup's, read inside its image
    /// (§5.4 of docs/06-storage.md), is one.
    ///
    /// # Errors
    /// The token could not be minted, or the session opened.
    pub async fn opened(
        c: &Connection,
        workload: &str,
        (action, params): (&'static str, Vec<u8>),
        at: Option<&str>,
    ) -> Result<Self, CliError> {
        let minted = mint(c, workload, action).await?;
        let session = open(&relay(c, at).await?, workload, &minted, params).await?;
        Ok(Self {
            c: c.clone(),
            workload: workload.to_owned(),
            session,
            minted,
            sent: 0,
            answered: 0,
        })
    }

    /// How many requests are sent and not yet answered.
    #[must_use]
    pub const fn ahead(&self) -> u64 {
        self.sent - self.answered
    }

    /// Sends `request`, ahead of the answers to those before it.
    ///
    /// # Errors
    /// The session is gone.
    pub async fn send(&mut self, request: Request) -> Result<(), CliError> {
        self.sent += 1;
        let r = pb::FileRequest { id: self.sent, request: Some(request), ..Default::default() };
        self.session.send(frame(Frame::Data(r.encode_to_vec()))).await?;
        Ok(())
    }

    /// The answer to the oldest request not yet answered, a refusal returned as the error it is.
    ///
    /// # Errors
    /// The node refused it, or the session ended.
    pub async fn answer(&mut self, path: &str) -> Result<Response, CliError> {
        let answer = loop {
            let heard = tokio::select! {
                m = self.session.message::<pb::RelayResponse>() => m?,
                () = tokio::time::sleep(self.minted.renew_in()) => {
                    renew(&self.c, &self.workload, &mut self.session, &mut self.minted).await?;
                    continue;
                }
            };
            let message = heard.map(|m| m.to_owned_message()).and_then(|m| m.message);
            let Some(Reply::Frame(f)) = message else {
                return Err(CliError::Transport("the session ended".into()));
            };
            match f.frame {
                Some(Frame::Data(bytes)) => {
                    break pb::FileResponse::decode_from_slice(&bytes)
                        .map_err(|e| CliError::Transport(format!("an answer: {e}")))?;
                }
                Some(Frame::End(end)) => {
                    let said = super::printable(&format!("{}: {}", end.code, end.message));
                    return Err(CliError::Transport(said));
                }
                _ => {}
            }
        };
        self.answered += 1;
        if answer.id != self.answered {
            return Err(CliError::Transport("an answer out of order".into()));
        }
        match answer.response {
            Some(Response::Error(e)) => Err(refused(path, &e)),
            Some(r) => Ok(r),
            None => Err(CliError::Transport("an empty answer".into())),
        }
    }

    /// Asks `request` and waits for its answer.
    ///
    /// # Errors
    /// The node refused it, or the session ended.
    pub async fn ask(&mut self, request: Request) -> Result<Response, CliError> {
        let path = path_of(&request).to_owned();
        self.send(request).await?;
        self.answer(&path).await
    }

    /// The metadata of `path`; none when it is not there.
    ///
    /// # Errors
    /// The node refused otherwise, or the session ended.
    pub async fn stat(&mut self, path: &str) -> Result<Option<pb::FileEntry>, CliError> {
        let stat = pb::FileStat { path: path.to_owned(), ..Default::default() };
        match self.ask(Request::Stat(Box::new(stat))).await {
            Ok(Response::Entry(e)) => Ok(Some(*e)),
            Err(CliError::Api { code: pb::ErrorCode::ERROR_CODE_NOT_FOUND, .. }) => Ok(None),
            Err(e) => Err(e),
            Ok(other) => Err(CliError::Transport(format!("a stat answered {other:?}"))),
        }
    }
}
