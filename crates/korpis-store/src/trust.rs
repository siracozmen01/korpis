// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipe trust policies (§7 of docs/09-recipes.md).

use crate::errors::from_sqlx;
use crate::stored::{count, from_json, from_time, principal, to_count, to_json, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{PolicyChange, SetPolicy, StoreResult, TrustRead, TrustWrite};
use korpis_core::recipe::trust::{Signer, TrustPolicy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SignerDoc {
    name: String,
    public_key_pem: String,
    fingerprint: String,
}

fn signer_docs(policy: &TrustPolicy) -> Vec<SignerDoc> {
    policy
        .accepted_signers
        .iter()
        .map(|s| SignerDoc {
            name: s.name.clone(),
            public_key_pem: s.public_key_pem.clone(),
            fingerprint: s.fingerprint.clone(),
        })
        .collect()
}

impl TrustRead for PgStore {
    async fn trust_policy(&self, organization: uuid::Uuid) -> StoreResult<Option<SetPolicy>> {
        let row = sqlx::query_file!("queries/trust_policy_effective.sql", organization)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)?;
        row.map(|r| {
            let signers: Vec<SignerDoc> = from_json("trust signers", r.accepted_signers)?;
            Ok(SetPolicy {
                organization: r.organization_id,
                policy: TrustPolicy {
                    accepted_signers: signers
                        .into_iter()
                        .map(|s| Signer {
                            name: s.name,
                            public_key_pem: s.public_key_pem,
                            fingerprint: s.fingerprint,
                        })
                        .collect(),
                    allow_unsigned: r.allow_unsigned,
                    allow_unverified: r.allow_unverified,
                },
                version: count("trust policy version", r.version)?,
                set_by: principal(&r.set_by)?,
                set_at: to_time(r.set_at),
            })
        })
        .transpose()
    }

    async fn own_trust_version(&self, organization: uuid::Uuid) -> StoreResult<u64> {
        let version =
            sqlx::query_file_scalar!("queries/trust_policy_own_version.sql", organization)
                .fetch_optional(&self.pool)
                .await
                .map_err(from_sqlx)?;
        version.map_or(Ok(0), |v| count("trust policy version", v))
    }
}

impl TrustWrite for PgTx {
    async fn set_trust_policy(&mut self, c: &PolicyChange, expected: u64) -> StoreResult<bool> {
        let empty = TrustPolicy::default();
        let policy = c.policy.as_ref().unwrap_or(&empty);
        let signers = to_json("trust signers", &signer_docs(policy))?;
        let version = to_count("trust policy version", c.version)?;
        let (by, at) = (c.set_by.key(), from_time(c.set_at));
        let (unsigned, unverified) = (policy.allow_unsigned, policy.allow_unverified);
        let (org, inherit) = (c.organization, c.policy.is_none());
        let query = if expected == 0 {
            if inherit {
                return Ok(false);
            }
            sqlx::query_file!(
                "queries/trust_policy_set.sql",
                org,
                signers,
                unsigned,
                unverified,
                version,
                by,
                at
            )
        } else {
            sqlx::query_file!(
                "queries/trust_policy_update.sql",
                org,
                signers,
                unsigned,
                unverified,
                version,
                by,
                at,
                to_count("trust policy version", expected)?,
                inherit
            )
        };
        let done = query.execute(&mut *self.tx).await.map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
