// SPDX-License-Identifier: AGPL-3.0-or-later

//! Relays that run as their own process, and the tokens that enroll them (§3.1 of
//! docs/02-architecture.md).

use crate::errors::from_sqlx;
use crate::stored::{from_time, principal, to_time};
use crate::tenancy::name;
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::ports::{
    RelayCertificate, RelayEnrollment, RelayRead, RelaySftp, RelayWrite, StoreError, StoreResult,
};
use korpis_core::event::Severity;
use korpis_core::relay::{CertificateAlert, Relay, RelayUrl};
use std::time::SystemTime;
use uuid::Uuid;

struct RelayRow {
    id: Uuid,
    name: String,
    url: String,
    joined_at: DateTime<Utc>,
    certificate_expires_at: Option<DateTime<Utc>>,
    removed_at: Option<DateTime<Utc>>,
    held_at: Option<DateTime<Utc>>,
    last_seen_at: Option<DateTime<Utc>>,
}

fn severity(s: &str) -> StoreResult<Severity> {
    Severity::parse(s).ok_or_else(|| StoreError::Corrupt(format!("certificate_alert {s:?}")))
}

fn url(s: &str) -> StoreResult<RelayUrl> {
    RelayUrl::parse(s).map_err(|e| StoreError::Corrupt(e.to_string()))
}

impl RelayRow {
    fn into_domain(self) -> StoreResult<Relay> {
        Ok(Relay {
            id: self.id,
            name: name(self.name)?,
            url: url(&self.url)?,
            joined_at: to_time(self.joined_at),
            certificate_expires_at: self.certificate_expires_at.map(to_time),
            removed_at: self.removed_at.map(to_time),
            held_at: self.held_at.map(to_time),
            last_seen_at: self.last_seen_at.map(to_time),
        })
    }
}

impl RelayRead for PgStore {
    async fn relays(&self) -> StoreResult<Vec<Relay>> {
        let rows = sqlx::query_as!(
            RelayRow,
            "SELECT id, name, url, joined_at, certificate_expires_at, removed_at, held_at,
                    last_seen_at
               FROM relay ORDER BY name"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(RelayRow::into_domain).collect()
    }

    async fn relay(&self, id: Uuid) -> StoreResult<Option<Relay>> {
        let row = sqlx::query_as!(
            RelayRow,
            "SELECT id, name, url, joined_at, certificate_expires_at, removed_at, held_at,
                    last_seen_at
               FROM relay WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(RelayRow::into_domain).transpose()
    }

    async fn relay_key(&self, id: Uuid) -> StoreResult<Option<Vec<u8>>> {
        sqlx::query_scalar!("SELECT public_key FROM relay WHERE id = $1", id)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)
    }

    async fn relay_sftp(&self) -> StoreResult<Vec<RelaySftp>> {
        crate::relay_sftp::read(self).await
    }

    async fn relay_certificates(&self) -> StoreResult<Vec<RelayCertificate>> {
        let rows = sqlx::query!(
            r#"SELECT r.id, r.name, r.url, r.joined_at, r.certificate_expires_at AS "expires_at!",
                      r.certificate_alert, r.held_at, r.last_seen_at, e.organization_id
                 FROM relay r JOIN relay_enrollment e ON e.relay_id = r.id
                WHERE r.removed_at IS NULL AND r.certificate_expires_at IS NOT NULL
                ORDER BY r.name"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let alerted = r.certificate_alert.as_deref().map(severity).transpose()?;
                let relay = RelayRow {
                    id: r.id,
                    name: r.name,
                    url: r.url,
                    joined_at: r.joined_at,
                    certificate_expires_at: Some(r.expires_at),
                    removed_at: None,
                    held_at: r.held_at,
                    last_seen_at: r.last_seen_at,
                };
                Ok(RelayCertificate {
                    relay: relay.into_domain()?,
                    organization: r.organization_id,
                    expires_at: to_time(r.expires_at),
                    alerted,
                })
            })
            .collect()
    }

    async fn relay_enrollment(&self, token: Uuid) -> StoreResult<Option<RelayEnrollment>> {
        let row = sqlx::query!(
            "SELECT token_id, grant_id, organization_id, relay_name, relay_url, created_by,
                    created_at, expires_at, relay_id
               FROM relay_enrollment WHERE token_id = $1",
            token
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            Ok(RelayEnrollment {
                token: r.token_id,
                grant: r.grant_id,
                organization: r.organization_id,
                relay_name: name(r.relay_name)?,
                relay_url: url(&r.relay_url)?,
                created_by: principal(&r.created_by)?,
                created_at: to_time(r.created_at),
                expires_at: to_time(r.expires_at),
                relay: r.relay_id,
            })
        })
        .transpose()
    }
}

impl RelayWrite for PgTx {
    async fn insert_relay_enrollment(&mut self, e: &RelayEnrollment) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO relay_enrollment (token_id, grant_id, organization_id, relay_name,
                                           relay_url, created_by, created_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            e.token,
            e.grant,
            e.organization,
            e.relay_name.as_str(),
            e.relay_url.as_str(),
            e.created_by.key(),
            from_time(e.created_at),
            from_time(e.expires_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn join_relay(&mut self, r: &Relay, public_key: &[u8], token: Uuid) -> StoreResult<bool> {
        sqlx::query!(
            "INSERT INTO relay (id, name, url, public_key, joined_at)
             VALUES ($1, $2, $3, $4, $5)",
            r.id,
            r.name.as_str(),
            r.url.as_str(),
            public_key,
            from_time(r.joined_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let done = sqlx::query!(
            "UPDATE relay_enrollment SET relay_id = $2, joined_at = $3
              WHERE token_id = $1 AND relay_id IS NULL",
            token,
            r.id,
            from_time(r.joined_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn record_relay_certificate(
        &mut self,
        relay: Uuid,
        expires_at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE relay SET certificate_expires_at = $2, certificate_alert = NULL WHERE id = $1",
            relay,
            from_time(expires_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn remember_relay_nonce(
        &mut self,
        relay: Uuid,
        nonce: &[u8],
        at: SystemTime,
        forget_before: SystemTime,
    ) -> StoreResult<bool> {
        sqlx::query!(
            "DELETE FROM relay_auth_nonce WHERE relay_id = $1 AND seen_at < $2",
            relay,
            from_time(forget_before)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let inserted = sqlx::query!(
            "INSERT INTO relay_auth_nonce (relay_id, nonce, seen_at) VALUES ($1, $2, $3)
             ON CONFLICT (relay_id, nonce) DO NOTHING",
            relay,
            nonce,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(inserted.rows_affected() == 1)
    }

    async fn alert_relay_certificate(
        &mut self,
        alert: &CertificateAlert,
        expected: Option<Severity>,
    ) -> StoreResult<bool> {
        let swapped = sqlx::query!(
            "UPDATE relay SET certificate_alert = $2
              WHERE id = $1 AND certificate_alert IS NOT DISTINCT FROM $3
                AND certificate_expires_at = $4",
            alert.relay,
            alert.severity.as_str(),
            expected.map(Severity::as_str),
            from_time(alert.expires_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        if swapped.rows_affected() != 1 {
            return Ok(false);
        }
        let detail = serde_json::json!({
            "relay": alert.relay.to_string(),
            "name": alert.name,
            "expires_at": crate::stored::from_time(alert.expires_at).to_rfc3339(),
        });
        sqlx::query!(
            "SELECT korpis_append_event($1, $2, $3, $4, $5, NULL, NULL, $6)",
            alert.key(),
            from_time(alert.at),
            CertificateAlert::KIND,
            alert.severity.as_str(),
            alert.organization,
            detail
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(true)
    }

    async fn set_relay_sftp(&mut self, sftp: &RelaySftp) -> StoreResult<()> {
        crate::relay_sftp::set(self, sftp).await
    }

    async fn clear_relay_sftp(&mut self, relay: Uuid) -> StoreResult<()> {
        crate::relay_sftp::clear(self, relay).await
    }

    async fn relay_seen(&mut self, relay: Uuid, at: SystemTime) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE relay SET last_seen_at = greatest(last_seen_at, $2) WHERE id = $1",
            relay,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn remove_relay(&mut self, relay: Uuid, at: SystemTime) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE relay SET removed_at = $2 WHERE id = $1 AND removed_at IS NULL",
            relay,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn hold_relays(&mut self, at: SystemTime) -> StoreResult<u64> {
        let done = sqlx::query!(
            "UPDATE relay SET held_at = $1 WHERE removed_at IS NULL AND held_at IS NULL",
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }

    async fn readmit_relay(&mut self, relay: Uuid) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE relay SET held_at = NULL
              WHERE id = $1 AND held_at IS NOT NULL AND removed_at IS NULL",
            relay
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
