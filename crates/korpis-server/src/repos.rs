// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's backup repository as the control plane reads it: through the access a target
//! granted, under the workload's key (§5.4 and §5.5 of docs/06-storage.md).

use korpis_app::backup::job::JobGrant;
use korpis_backup::s3::{S3Config, S3Store};
use korpis_backup::store::StoreError;
use korpis_backup::{Keys, Repo};
use std::sync::Arc;

/// The repository a grant reaches.
///
/// # Errors
/// The access names a store that cannot be addressed.
pub(crate) fn repo_of(g: &JobGrant) -> Result<Repo<S3Store>, StoreError> {
    let s = &g.access.s3;
    let config = S3Config {
        endpoint: s.endpoint.clone(),
        region: s.region.clone(),
        bucket: s.bucket.clone(),
        key_prefix: s.key_prefix.clone(),
        access_key_id: s.access_key_id.clone(),
        secret_access_key: s.secret_access_key.clone(),
        session_token: s.session_token.clone(),
        path_style: s.path_style,
    };
    let store = S3Store::new(config)?;
    Ok(Repo::new(Arc::new(store), Arc::new(Keys::derive(&g.data_key.0))))
}
