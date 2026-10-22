// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where an account's shared secrets were set (migration 0114).

use crate::PgTx;
use crate::errors::from_sqlx;
use korpis_app::ports::StoreResult;
use korpis_core::account::exposed::{SetWithin, Shared};
use uuid::Uuid;

pub(super) async fn set_within(pool: &sqlx::PgPool, account: Uuid) -> StoreResult<SetWithin> {
    let row = sqlx::query!(
        "SELECT password_set_within, totp_set_within, recovery_codes_set_within
           FROM account WHERE id = $1",
        account
    )
    .fetch_optional(pool)
    .await
    .map_err(from_sqlx)?;
    Ok(row.map_or_else(SetWithin::default, |r| SetWithin {
        password: r.password_set_within,
        totp: r.totp_set_within,
        recovery_codes: r.recovery_codes_set_within,
    }))
}

pub(super) async fn record(
    tx: &mut PgTx,
    account: Uuid,
    secret: Shared,
    within: Option<Uuid>,
) -> StoreResult<()> {
    let query = match secret {
        Shared::Password => {
            sqlx::query!(
                "UPDATE account SET password_set_within = $2 WHERE id = $1",
                account,
                within
            )
        }
        Shared::Totp => {
            sqlx::query!("UPDATE account SET totp_set_within = $2 WHERE id = $1", account, within)
        }
        Shared::RecoveryCodes => sqlx::query!(
            "UPDATE account SET recovery_codes_set_within = $2 WHERE id = $1",
            account,
            within
        ),
    };
    query.execute(&mut *tx.tx).await.map_err(from_sqlx)?;
    Ok(())
}
