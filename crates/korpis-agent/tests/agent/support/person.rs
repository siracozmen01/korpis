// SPDX-License-Identifier: AGPL-3.0-or-later

//! A person holding an SSH key, for SFTP (§9.9 of docs/08-identity.md): invited with a grant, as
//! an owner invites one, and the key written to the store as the profile registers it, since
//! registering it through the API needs a sign-in this harness does not make.

use super::{ControlPlane, mutation};
use korpis_core::account::ssh_key::SshPublicKey;
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::v1 as pb;

impl ControlPlane {
    /// A person invited with `actions` on the root organization, and the public key `line`, as
    /// `ssh-keygen` writes it, registered to them; their account.
    pub async fn person_with_key(&self, key: &str, actions: &[&str], line: &str) -> uuid::Uuid {
        use korpis_core::authz::{Kind, ObjectRef};
        let org = Id::parse(IdKind::Organization, &self.root_org).unwrap().uuid();
        let scope = korpis_core::scope::Scope::Object(ObjectRef::new(Kind::Organization, org));
        let issued = pb::GrantServiceClient::new(self.http(), self.config())
            .issue_grant(pb::IssueGrantRequest {
                mutation: mutation(key).into(),
                actions: actions.iter().map(|a| (*a).to_owned()).collect(),
                scope: korpis_wire::scope::to_pb(&scope).into(),
                to_new_user: pb::NewUser { display_name: key.into(), ..Default::default() }.into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned();
        let subject = issued.grant.into_option().unwrap().subject.into_option().unwrap();
        let Some(pb::subject::Kind::User(user)) = subject.kind else { panic!("a person") };
        let account = Id::parse(IdKind::User, &user).unwrap().uuid();
        let k = SshPublicKey::parse(line).unwrap();
        sqlx::query(
            "INSERT INTO ssh_key (id, account_id, kind, public_key, fingerprint, label, added_at)
             VALUES ($1, $2, $3, $4, $5, $6, now())",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(account)
        .bind(k.kind().name())
        .bind(k.blob().to_vec())
        .bind(k.fingerprint())
        .bind(key)
        .execute(self.store().await.pool())
        .await
        .unwrap();
        account
    }
}
