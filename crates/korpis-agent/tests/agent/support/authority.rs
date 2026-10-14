// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the tests need of the control plane's authority: grants issued and revoked, tokens
//! minted, and the relay a client opens sessions at.

use super::{ControlPlane, mutation};
use connectrpc::client::{ClientConfig, HttpClient};
use korpis_proto::korpis::v1 as pb;

impl ControlPlane {
    /// The relay service, as an anonymous client: a session is authorized by its token alone.
    pub fn relay(&self) -> pb::RelayServiceClient<HttpClient> {
        pb::RelayServiceClient::new(self.http(), ClientConfig::new(self.url.parse().unwrap()))
    }

    /// A capability token for `actions` on exactly `workload`, minted by the root token.
    pub async fn mint(&self, workload: &str, actions: &[&str]) -> String {
        self.mint_as(&self.secret, workload, actions, 0).await
    }

    /// A capability token for `actions` on exactly `workload`, minted by whoever `secret`
    /// authenticates, lasting `ttl_seconds` (the server's maximum when zero).
    pub async fn mint_as(
        &self,
        secret: &str,
        workload: &str,
        actions: &[&str],
        ttl: u32,
    ) -> String {
        self.minted(secret, workload, actions, ttl).await.token
    }

    /// The whole answer to minting, the sealed route with the token.
    pub async fn minted(
        &self,
        secret: &str,
        workload: &str,
        actions: &[&str],
        ttl: u32,
    ) -> pb::MintTokenResponse {
        use korpis_core::authz::{Kind, ObjectRef};
        use korpis_core::id::{Id, IdKind};
        let w = Id::parse(IdKind::Workload, workload).unwrap().uuid();
        let scope = korpis_core::scope::Scope::Object(ObjectRef::new(Kind::Workload, w));
        let config = ClientConfig::new(self.url.parse().unwrap())
            .with_default_header("authorization", format!("Bearer {secret}"));
        pb::GrantServiceClient::new(self.http(), config)
            .mint_token(pb::MintTokenRequest {
                actions: actions.iter().map(|a| (*a).to_owned()).collect(),
                scope: korpis_wire::scope::to_pb(&scope).into(),
                ttl_seconds: ttl,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned()
    }

    /// Issues `actions` on the root organization to a new API token; the grant and the token's
    /// secret.
    pub async fn grant_to_new_token(&self, key: &str, actions: &[&str]) -> (String, String) {
        use korpis_core::authz::{Kind, ObjectRef};
        use korpis_core::id::{Id, IdKind};
        let org = Id::parse(IdKind::Organization, &self.root_org).unwrap().uuid();
        let scope = korpis_core::scope::Scope::Object(ObjectRef::new(Kind::Organization, org));
        let issued = pb::GrantServiceClient::new(self.http(), self.config())
            .issue_grant(pb::IssueGrantRequest {
                mutation: mutation(key).into(),
                actions: actions.iter().map(|a| (*a).to_owned()).collect(),
                scope: korpis_wire::scope::to_pb(&scope).into(),
                label: key.into(),
                to_new_token: true,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned();
        (issued.grant.as_option().unwrap().id.clone(), issued.token)
    }

    /// Revokes `grant`; its identifier.
    pub async fn revoke(&self, grant: &str) -> uuid::Uuid {
        use korpis_core::id::{Id, IdKind};
        pb::GrantServiceClient::new(self.http(), self.config())
            .revoke_grant(pb::RevokeGrantRequest {
                mutation: mutation(&format!("revoke-{grant}")).into(),
                grant: grant.to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
        Id::parse(IdKind::Grant, grant).unwrap().uuid()
    }

    /// Issues a grant on the root organization to a new token, then revokes it; the grant.
    pub async fn revoked_grant(&self) -> uuid::Uuid {
        let (grant, _) = self.grant_to_new_token("to-revoke", &["org.read"]).await;
        self.revoke(&grant).await
    }
}
