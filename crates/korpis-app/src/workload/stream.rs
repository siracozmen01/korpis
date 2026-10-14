// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a workload's stream as the control plane knows it (§4 of docs/14-streams.md): the
//! index of its segments, what is held only on its node, what is lost, and the limits it is
//! written to. Never its records, which the node serves over the data path.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::spec::{ConsoleMode, StreamSpec};
use korpis_core::workload::stream::{Stream, StreamKind};
use uuid::Uuid;

/// A stream as a client renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamView {
    /// The stream and its index; its identifier is nil until its first segment is sealed, when
    /// the index first hears of it.
    pub stream: Stream,
    /// The limits it is written to, from the intent its node runs.
    pub limits: StreamSpec,
    /// The operator's floor on how long offloaded segments are kept, disclosed with the stream
    /// (§8 of docs/17-security.md); zero is none.
    pub floor: std::time::Duration,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The workload's stream of `kind`. Reading it takes `workload.console.read`, the authority
    /// to read the records themselves: how much a workload wrote, and when, is part of what it
    /// wrote. A workload whose console mode is `none` has no stream, and is not found.
    pub async fn get_stream(
        &self,
        caller: &Caller,
        workload: Uuid,
        kind: StreamKind,
    ) -> Result<StreamView, AppError> {
        let on = ObjectRef::new(Kind::Workload, workload);
        self.authorize(caller, "workload.console.read", on).await?;
        let not_found = AppError::NotFound { kind: "stream" };
        let w = self.store.workload(workload).await?.ok_or(not_found.clone())?;
        let runs = w.target.unwrap_or(w.intent);
        let intent = self.store.intent(runs).await?.ok_or_else(|| {
            AppError::Internal(format!("workload {workload} names intent {runs}, which is gone"))
        })?;
        let indexed = self.store.stream(workload, kind).await?;
        if indexed.is_none()
            && (kind != StreamKind::Console || intent.body.console == ConsoleMode::None)
        {
            return Err(not_found);
        }
        let stream =
            indexed.unwrap_or(Stream { id: Uuid::nil(), workload, kind, segments: Vec::new() });
        Ok(StreamView { stream, limits: intent.body.streams, floor: self.stream_floor })
    }
}
