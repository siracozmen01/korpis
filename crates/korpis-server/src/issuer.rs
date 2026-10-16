// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's issuer of `acme` certificates (§3.2 of docs/07-networking.md): whenever a
//! certificate is made, and every ten seconds besides, it issues each certificate due, a few at
//! once, withdraws the DNS challenge records an issuer that stopped mid-order left standing, and
//! removes the certificates nothing names. At
//! start it makes every certificate not issued due at once, so a setting the operator changed,
//! the directory's terms agreed for one, is tried without waiting out a backoff. Each certificate
//! is taken under a lease, so replicas share the work rather than repeat it.

use crate::api::Api;
use korpis_app::ports::{Store as _, Timer, Wakeups as _};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// How many orders run at once.
const AT_ONCE: usize = 4;

/// Tokio's timer, for the use cases that wait.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioTimer;

impl Timer for TokioTimer {
    fn sleep(&self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(d))
    }
}

/// Issues until nothing is due, `AT_ONCE` orders at a time.
async fn sweep(api: &Arc<Api>) {
    loop {
        let mut orders = tokio::task::JoinSet::new();
        for _ in 0..AT_ONCE {
            let api = Arc::clone(api);
            orders.spawn(async move { api.app().issue_next().await });
        }
        let mut issued = false;
        while let Some(done) = orders.join_next().await {
            match done {
                Ok(Ok(one)) => issued |= one,
                Ok(Err(e)) => tracing::warn!(error = %e, "a certificate could not be issued"),
                Err(e) => tracing::error!(error = %e, "an issuance ended abnormally"),
            }
        }
        if !issued {
            return;
        }
    }
}

/// Runs the issuer; the task ends with the process.
pub async fn keep(api: Arc<Api>) {
    if let Err(e) = api.app().rearm_certificates().await {
        tracing::warn!(error = %e, "the certificates waiting could not be made due");
    }
    let mut wakeups = api.app().store().certificates();
    loop {
        sweep(&api).await;
        match api.app().withdraw_stale_records().await {
            Ok(0) => {}
            Ok(n) => tracing::info!(records = n, "challenge records left standing withdrawn"),
            Err(e) => tracing::warn!(error = %e, "withdrawing challenge records failed"),
        }
        match api.app().remove_orphan_certificates().await {
            Ok(0) => {}
            Ok(n) => tracing::info!(certificates = n, "certificates nothing names removed"),
            Err(e) => tracing::warn!(error = %e, "removing certificates nothing names failed"),
        }
        if wakeups.next().await.is_none() {
            return;
        }
    }
}
