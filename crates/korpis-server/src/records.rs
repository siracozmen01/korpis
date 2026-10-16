// SPDX-License-Identifier: AGPL-3.0-or-later

//! The reconciler of the names of ingress endpoints in DNS (§3.5 of docs/07-networking.md):
//! whenever an endpoint is placed, moved, or removed, or a node's advertised addresses change,
//! and every thirty seconds besides, it makes the recorded names follow the endpoints the ingress
//! serves and publishes or withdraws each name due, a few at once. Each name is taken under a
//! lease, so replicas share the work rather than repeat it.

use crate::api::Api;
use korpis_app::ports::{Store as _, Wakeups as _};
use std::sync::Arc;

/// How many names are published at once.
const AT_ONCE: usize = 4;

/// Publishes until nothing is due, `AT_ONCE` names at a time.
async fn sweep(api: &Arc<Api>) {
    loop {
        let mut names = tokio::task::JoinSet::new();
        for _ in 0..AT_ONCE {
            let api = Arc::clone(api);
            names.spawn(async move { api.app().publish_next_record().await });
        }
        let mut published = false;
        while let Some(done) = names.join_next().await {
            match done {
                Ok(Ok(one)) => published |= one,
                Ok(Err(e)) => tracing::warn!(error = %e, "an endpoint's name was not published"),
                Err(e) => tracing::error!(error = %e, "publishing a name ended abnormally"),
            }
        }
        if !published {
            return;
        }
    }
}

/// Runs the reconciler; the task ends with the process.
pub async fn keep(api: Arc<Api>) {
    let mut wakeups = api.app().store().endpoint_changes();
    loop {
        match api.app().sync_endpoint_records().await {
            Ok(()) => sweep(&api).await,
            Err(e) => tracing::warn!(error = %e, "the names of endpoints could not be followed"),
        }
        if wakeups.next().await.is_none() {
            return;
        }
    }
}
