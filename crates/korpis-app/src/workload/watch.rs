// SPDX-License-Identifier: AGPL-3.0-or-later

//! Watching one workload (§4.3 of docs/02-architecture.md): its status now, then each status that
//! differs from the last one sent, read when the workload is announced as changed and on every
//! resync. Each is read with the caller's authority as it is at that moment, so a grant revoked
//! during a watch ends it with the same refusal a fresh read would get. Opening the watch is the
//! read a use-limited grant pays for; each later read continues it through the grant that paid
//! and spends nothing, as a session's renewals do (§6.1 of docs/08-identity.md), or a three-use
//! share link would watch three changes and go dark.

use super::read::WorkloadView;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Change, Changes, Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use uuid::Uuid;

/// A watch in progress.
#[derive(Debug)]
pub struct Watch<X> {
    workload: Uuid,
    /// The grant that authorized opening it, which every later read continues through.
    through: Uuid,
    changes: X,
    last: WorkloadView,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Starts watching a workload the caller may read, returning its status now. The
    /// subscription is taken before that read, so no change between the two is lost.
    pub async fn watch_workload(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<(WorkloadView, Watch<S::Changes>), AppError> {
        let changes = self.store.subscribe();
        let object = ObjectRef::new(Kind::Workload, id);
        let permit = self.authorize(caller, "workload.read", object).await?;
        let view = self.workload_view(caller, id).await?;
        Ok((view.clone(), Watch { workload: id, through: permit.grant, changes, last: view }))
    }

    /// The next status that differs from the last one returned; `None` when the feed has ended.
    pub async fn next_status(
        &self,
        caller: &Caller,
        watch: &mut Watch<S::Changes>,
    ) -> Result<Option<WorkloadView>, AppError> {
        loop {
            let relevant = match watch.changes.next().await {
                None => return Ok(None),
                Some(Change::Workload(id)) => id == watch.workload,
                Some(Change::Resync) => true,
            };
            if !relevant {
                continue;
            }
            let object = ObjectRef::new(Kind::Workload, watch.workload);
            self.authorize_continued(caller, "workload.read", object, watch.through).await?;
            let view = self.workload_view(caller, watch.workload).await?;
            if view != watch.last {
                watch.last = view.clone();
                return Ok(Some(view));
            }
        }
    }
}
