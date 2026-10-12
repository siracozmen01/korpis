// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading the effect log (§4 of docs/03-state.md, §10 of docs/10-api.md).

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, EffectCursor, EffectFilter, Store};
use korpis_core::audit::Effect;
use korpis_core::authz::{Kind, ObjectRef};
use std::time::SystemTime;
use uuid::Uuid;

/// Whose effects to list. The log is read through what it concerns, so reading it is gated by
/// reading that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectsOf {
    /// A workload's; needs `workload.read`.
    Workload(Uuid),
    /// A node's; needs `node.read`.
    Node(Uuid),
}

/// A page of the log.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectPage {
    /// The entries, in log order.
    pub items: Vec<Effect>,
    /// Where the next page starts; `None` on the last page.
    pub next: Option<EffectCursor>,
}

impl<S: Store, C: Clock> App<S, C> {
    /// Effects concerning one workload or node, in log order.
    pub async fn list_effects(
        &self,
        caller: &Caller,
        of: EffectsOf,
        since: Option<SystemTime>,
        after: Option<EffectCursor>,
        size: u32,
    ) -> Result<EffectPage, AppError> {
        let mut filter = EffectFilter { since, ..EffectFilter::default() };
        match of {
            EffectsOf::Workload(id) => {
                self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, id)).await?;
                filter.workload = Some(id);
            }
            EffectsOf::Node(id) => {
                self.authorize(caller, "node.read", ObjectRef::new(Kind::Node, id)).await?;
                filter.node = Some(id);
            }
        }
        let page = Page::new(None, size);
        let items = self.store.effects(&filter, after, page.size).await?;
        let full = u32::try_from(items.len()).unwrap_or(u32::MAX) >= page.size;
        let next =
            items.last().filter(|_| full).and_then(|e| Some(EffectCursor { at: e.at?, id: e.id }));
        Ok(EffectPage { items, next })
    }
}
