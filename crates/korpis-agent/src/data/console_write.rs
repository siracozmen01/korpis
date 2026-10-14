// SPDX-License-Identifier: AGPL-3.0-or-later

//! Writing to a console over the data path: the handler of `workload.console.write` (§6 of
//! docs/14-streams.md). Each frame the client sends is a `ConsoleInput`: bytes for the workload,
//! or a new terminal size. Input is an effect before it is anything else: the node journals that
//! the session's subject sent so many bytes on this session, under the grant its token was minted
//! from, and only then hands the bytes to the driver, which records them in the stream with who
//! sent them and feeds the workload. So an input the workload received is never one the journal
//! does not hold, and "who stopped the server" is a row.
//!
//! The control plane believes the effect only as far as the grant named permitted its subject
//! the input when it happened (`korpis-app`'s attestation), so a node can attribute input only to
//! someone who could have given it.

use super::handler::{Ended, Handler, Io, Opened, Serving};
use crate::journal::JournalHandle;
use crate::runtime::{Runtime, RuntimeError, Sender};
use buffa::Message as _;
use korpis_core::principal::Principal;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::console_input::Input;
use std::collections::BTreeMap;
use tokio::sync::watch;

/// The action.
pub const ACTION: &str = "workload.console.write";

/// The runtime object of each workload the node runs, as the reconciler last knew it.
pub type Runtimes = watch::Receiver<BTreeMap<String, String>>;

/// Serves `workload.console.write` through the node's runtime driver.
#[derive(Debug, Clone)]
pub struct ConsoleWrite<R> {
    runtime: R,
    runtimes: Runtimes,
    journal: JournalHandle,
}

impl<R: Runtime + Clone> ConsoleWrite<R> {
    /// Writes through `runtime` to the objects `runtimes` names, journaling each input.
    #[must_use]
    pub const fn new(runtime: R, runtimes: Runtimes, journal: JournalHandle) -> Self {
        Self { runtime, runtimes, journal }
    }
}

impl<R: Runtime + Clone> Handler for ConsoleWrite<R> {
    fn serve(&self, opened: Opened, io: Io) -> Serving {
        Box::pin(serve(self.clone(), opened, io))
    }
}

fn ended(code: &str, message: impl Into<String>) -> Ended {
    Ended { code: code.to_owned(), message: message.into() }
}

fn refused(e: RuntimeError) -> Ended {
    match e {
        RuntimeError::Unavailable(m) => ended("unavailable", m),
        RuntimeError::NotFound(m) => ended("unavailable", format!("not running here: {m}")),
        RuntimeError::Failed(m) => ended("refused", m),
    }
}

fn text(s: &str) -> pb::Value {
    pb::Value { kind: Some(pb::value::Kind::StringValue(s.to_owned())), ..Default::default() }
}

/// The effect of `bytes` of input from `opened`'s subject.
fn effect(opened: &Opened, actor: &Principal, bytes: usize) -> pb::Effect {
    let mut after = BTreeMap::new();
    after.insert("session".to_owned(), text(&opened.session));
    let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
    let number = pb::Value { kind: Some(pb::value::Kind::IntValue(bytes)), ..Default::default() };
    after.insert("bytes".to_owned(), number);
    pb::Effect {
        workload: Some(opened.workload.clone()),
        action: ACTION.to_owned(),
        outcome: pb::EffectOutcome::EFFECT_OUTCOME_SUCCEEDED.into(),
        actor: korpis_wire::principal::to_pb(actor).into(),
        grant: opened
            .chain
            .first()
            .map(|g| korpis_wire::wire_id(korpis_core::id::IdKind::Grant, *g)),
        after,
        ..Default::default()
    }
}

impl<R: Runtime + Clone> ConsoleWrite<R> {
    /// Takes one input from the client.
    async fn take(&self, opened: &Opened, actor: &Principal, input: Input) -> Result<(), Ended> {
        let id = self.runtimes.borrow().get(&opened.workload).cloned();
        let id = id.ok_or_else(|| ended("unavailable", "the workload is not running here"))?;
        let from = Sender { by: &opened.subject, session: &opened.session };
        match input {
            Input::Data(data) => {
                let e = effect(opened, actor, data.len());
                self.journal.append(Body::Effect(Box::new(e))).await.map_err(|e| {
                    ended("unavailable", format!("the input could not be recorded: {e}"))
                })?;
                self.runtime.console_input(&id, from, &data).await.map_err(refused)
            }
            Input::Resize(size) => {
                let cols = u16::try_from(size.cols).unwrap_or(u16::MAX);
                let rows = u16::try_from(size.rows).unwrap_or(u16::MAX);
                self.runtime.resize_console(&id, from, (cols, rows)).await.map_err(refused)
            }
        }
    }
}

async fn serve<R: Runtime + Clone>(
    writer: ConsoleWrite<R>,
    opened: Opened,
    mut io: Io,
) -> Result<(), Ended> {
    let actor = Principal::from_key(&opened.subject)
        .map_err(|e| ended("refused", format!("the session's subject: {e}")))?;
    while let Some(bytes) = io.incoming.recv().await {
        let frame = pb::ConsoleInput::decode_from_slice(&bytes)
            .map_err(|e| ended("invalid", format!("a frame is not a ConsoleInput: {e}")))?;
        let Some(input) = frame.input else { continue };
        writer.take(&opened, &actor, input).await?;
    }
    Ok(())
}
