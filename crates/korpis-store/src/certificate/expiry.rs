// SPDX-License-Identifier: AGPL-3.0-or-later

//! Certificates nearing expiry (migration 0061): their validity read with the alert last
//! reported, and an alert recorded by swapping it, so two replicas looking at once report it once.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use korpis_app::ports::{Expiry, StoreError, StoreResult};
use korpis_core::dns::certificate::{ExpiryAlert, Source};
use korpis_core::event::Severity;

pub(super) async fn certificate_expiries(pool: &sqlx::PgPool) -> StoreResult<Vec<Expiry>> {
    let rows = sqlx::query!(
        r#"SELECT id, organization_id, names, source, not_before AS "not_before!",
                  not_after AS "not_after!", expiry_alert
             FROM certificate
            WHERE state = 'issued' AND not_before IS NOT NULL AND not_after IS NOT NULL
            ORDER BY not_after"#
    )
    .fetch_all(pool)
    .await
    .map_err(from_sqlx)?;
    rows.into_iter()
        .map(|r| {
            let corrupt = |what: &str| StoreError::Corrupt(format!("certificate {}: {what}", r.id));
            Ok(Expiry {
                certificate: r.id,
                organization: r.organization_id,
                names: r.names,
                source: Source::parse(&r.source).ok_or_else(|| corrupt("source"))?,
                not_before: to_time(r.not_before),
                not_after: to_time(r.not_after),
                alerted: r
                    .expiry_alert
                    .map(|a| Severity::parse(&a).ok_or_else(|| corrupt("expiry_alert")))
                    .transpose()?,
            })
        })
        .collect()
}

pub(super) async fn alert_certificate(
    tx: &mut PgTx,
    alert: &ExpiryAlert,
    expected: Option<Severity>,
) -> StoreResult<bool> {
    let swapped = sqlx::query!(
        "UPDATE certificate SET expiry_alert = $2
          WHERE id = $1 AND expiry_alert IS NOT DISTINCT FROM $3 AND not_after = $4",
        alert.certificate,
        alert.severity.as_str(),
        expected.map(Severity::as_str),
        from_time(alert.not_after)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    if swapped.rows_affected() != 1 {
        return Ok(false);
    }
    let detail = serde_json::json!({
        "certificate": alert.certificate.to_string(),
        "names": alert.names,
        "source": alert.source.as_str(),
        "not_after": chrono::DateTime::<chrono::Utc>::from(alert.not_after).to_rfc3339(),
    });
    sqlx::query!(
        "SELECT korpis_append_event($1, $2, $3, $4, $5, NULL, NULL, $6)",
        alert.key(),
        from_time(alert.at),
        ExpiryAlert::KIND,
        alert.severity.as_str(),
        alert.organization,
        detail
    )
    .fetch_one(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(true)
}
