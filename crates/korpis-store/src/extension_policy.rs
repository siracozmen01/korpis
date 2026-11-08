// SPDX-License-Identifier: AGPL-3.0-or-later

//! Root organizations' extension policies (migration 0150).

use crate::errors::from_sqlx;
use crate::stored::{count, from_json, from_time, principal, to_count, to_json, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{
    ExtensionPolicyRead, ExtensionPolicyWrite, SetExtensionPolicy, StoreResult,
};
use korpis_core::extension::policy::ExtensionPolicy;
use korpis_core::recipe::trust::Signer;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PublisherDoc {
    name: String,
    public_key_pem: String,
    fingerprint: String,
}

impl ExtensionPolicyRead for PgStore {
    async fn extension_policy(&self, organization: Uuid) -> StoreResult<Option<SetExtensionPolicy>> {
        let row = sqlx::query!(
            "SELECT organization_id, registries, publishers, tenants_install, version, set_by,
                    set_at
               FROM extension_policy WHERE organization_id = $1",
            organization
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            let publishers: Vec<PublisherDoc> = from_json("extension publishers", r.publishers)?;
            Ok(SetExtensionPolicy {
                organization: r.organization_id,
                policy: ExtensionPolicy {
                    registries: r.registries,
                    publishers: publishers
                        .into_iter()
                        .map(|p| Signer {
                            name: p.name,
                            public_key_pem: p.public_key_pem,
                            fingerprint: p.fingerprint,
                        })
                        .collect(),
                    tenants_install: r.tenants_install,
                },
                version: count("extension policy version", r.version)?,
                set_by: principal(&r.set_by)?,
                set_at: to_time(r.set_at),
            })
        })
        .transpose()
    }
}

impl ExtensionPolicyWrite for PgTx {
    async fn set_extension_policy(
        &mut self,
        s: &SetExtensionPolicy,
        expected: u64,
    ) -> StoreResult<bool> {
        let docs: Vec<PublisherDoc> = s
            .policy
            .publishers
            .iter()
            .map(|p| PublisherDoc {
                name: p.name.clone(),
                public_key_pem: p.public_key_pem.clone(),
                fingerprint: p.fingerprint.clone(),
            })
            .collect();
        let publishers = to_json("extension publishers", &docs)?;
        let version = to_count("extension policy version", s.version)?;
        let (org, by, at) = (s.organization, s.set_by.key(), from_time(s.set_at));
        let done = if expected == 0 {
            sqlx::query!(
                "INSERT INTO extension_policy (organization_id, registries, publishers,
                                               tenants_install, version, set_by, set_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (organization_id) DO NOTHING",
                org,
                &s.policy.registries,
                publishers,
                s.policy.tenants_install,
                version,
                by,
                at
            )
            .execute(&mut *self.tx)
            .await
        } else {
            sqlx::query!(
                "UPDATE extension_policy
                    SET registries = $2, publishers = $3, tenants_install = $4, version = $5,
                        set_by = $6, set_at = $7
                  WHERE organization_id = $1 AND version = $8",
                org,
                &s.policy.registries,
                publishers,
                s.policy.tenants_install,
                version,
                by,
                at,
                to_count("extension policy version", expected)?
            )
            .execute(&mut *self.tx)
            .await
        }
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
