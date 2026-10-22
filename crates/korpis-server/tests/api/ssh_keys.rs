// SPDX-License-Identifier: AGPL-3.0-or-later

//! SSH keys and the file tokens a relay serving SFTP asks for with them (§9.9 of
//! docs/08-identity.md): a person registers keys they prove they hold, one installation-wide per
//! fingerprint, and a relay is given, for a key its client holds, a token for what that person's
//! grants allow on the workload, writing or else reading, recorded under the relay's name, and
//! nothing for a key no one holds, once it is removed, or while its account is disabled.

use crate::lifecycle::workload;
use crate::panel::{access, browser, invite, redeem_keeping_cookie};
use crate::refusal::reason;
use crate::relays::token as relay_token;
use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use connectrpc::client::HttpClient;
use korpis_core::account::ssh_key::SshPublicKey;
use korpis_core::account::ssh_key::signing::ed25519_possession;
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::relay::v1 as relay;
use korpis_proto::korpis::v1 as pb;
use korpis_server::standalone::feed::{FilesAsked, signed_files_request};
use korpis_server::standalone::{RelayDir, join};

const RSA_2048: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCmA8kPHkrr+ucw5DrC9rSBi/+LbyMwwn2mQ\
    zGq51Lpkokwq9Rt4d8S2sdVvOThXVzI3M/Hf6HxhLfkXTYNRHeTsgzwM9eiKv+BZLfjlmAXaJqgf6SJrOyfdWlHIRSXR1U\
    9CwVChAKEki+LL9TFX361dGHolEz0nAepDH+unhO1xppDTiAnGurEJryalkG5lagaMZcV6p6sNPgcIDXeG7pnWz49Lx+pN\
    JWRjkg/DiwIkyOy9FR/WR2yZ3IXjlC+sAPuN0YxFS/DCwiq4vYx09Fl0+tN+t+nfqV8vvugKB6JvO9463uOzCCLMveel7T\
    TcmbeuenCktgYOF19wuPq7XwJ small";

fn profile(h: &Harness, bearer: &str) -> pb::ProfileServiceClient<HttpClient> {
    pb::ProfileServiceClient::new(h.http.clone(), h.config(Some(bearer)))
}

/// The key `seed` makes, added to `account` by whoever holds it.
fn add(seed: u8, account: &str, label: &str) -> pb::AddSshKeyRequest {
    let (key, signature) = ed25519_possession([seed; 32], account);
    pb::AddSshKeyRequest { key, label: label.into(), signature, ..Default::default() }
}

/// A person signed in, invited as `who`; their account and an access token of their session.
async fn person(h: &Harness, who: &str) -> (String, String) {
    let invitation = invite(h, &format!("invite-{who}"), who).await.invitation;
    let (redeemed, cookie) = redeem_keeping_cookie(h, &invitation, who, &browser()).await.unwrap();
    (redeemed.account, access(h, &cookie).await)
}

#[tokio::test]
async fn a_person_adds_lists_and_removes_ssh_keys_each_registered_once_by_its_holder() {
    let h = Harness::new().await;
    let (ada, bearer) = person(&h, "ada").await;
    let me = profile(&h, &bearer);
    let added = me.add_ssh_key(add(1, &ada, "laptop")).await.unwrap().into_owned();
    let key = added.key.as_option().unwrap().clone();
    assert!(key.id.starts_with("ssk_"));
    let line = ed25519_possession([1; 32], &ada).0;
    assert_eq!(key.fingerprint, SshPublicKey::parse(&line).unwrap().fingerprint());
    assert_eq!((key.kind.as_str(), key.label.as_str()), ("ssh-ed25519", "laptop"));
    let again = me.add_ssh_key(add(1, &ada, "again")).await.unwrap_err();
    assert_eq!(reason(&again), "ssh_key.taken");
    let weak = pb::AddSshKeyRequest { key: RSA_2048.into(), ..add(2, &ada, "old") };
    let weak = me.add_ssh_key(weak).await.unwrap_err();
    assert_eq!(weak.code, ErrorCode::InvalidArgument, "an RSA key below 3,072 bits");

    // A public key is public: naming one, with a signature for someone else, adds nothing.
    let (_, eve_bearer) = person(&h, "eve").await;
    let eve = profile(&h, &eve_bearer);
    let squatted = eve.add_ssh_key(add(3, &ada, "not mine")).await.unwrap_err();
    assert_eq!(squatted.code, ErrorCode::InvalidArgument, "signed for another account");
    let unsigned = pb::AddSshKeyRequest { signature: String::new(), ..add(3, &ada, "x") };
    assert_eq!(eve.add_ssh_key(unsigned).await.unwrap_err().code, ErrorCode::InvalidArgument);
    me.add_ssh_key(add(3, &ada, "ada's own")).await.unwrap();

    let listed = me.list_ssh_keys(pb::ListSshKeysRequest::default()).await.unwrap();
    assert_eq!(listed.into_owned().keys.len(), 2);
    let remove = pb::RemoveSshKeyRequest { id: key.id, ..Default::default() };
    me.remove_ssh_key(remove).await.unwrap();
    let listed = me.list_ssh_keys(pb::ListSshKeysRequest::default()).await.unwrap();
    assert_eq!(listed.into_owned().keys.len(), 1);
    assert_eq!(h.effects("account.ssh_key_added").await.len(), 2);
    assert_eq!(h.effects("account.ssh_key_removed").await.len(), 1);

    let token = profile(&h, "").list_ssh_keys(pb::ListSshKeysRequest::default()).await;
    assert!(token.is_err(), "no one signed in holds no keys");
}

/// Issues `actions` on the root organization to `account`.
async fn grant(h: &Harness, key: &str, account: &str, actions: &[&str]) {
    let subject = pb::Subject {
        kind: Some(pb::subject::Kind::User(account.to_owned())),
        ..Default::default()
    };
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation(key).into(),
            subject: subject.into(),
            actions: actions.iter().map(|a| (*a).to_owned()).collect(),
            scope: org_scope(&h.root_org()).into(),
            ..Default::default()
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_relay_is_given_a_file_token_for_a_key_on_what_its_person_may_reach() {
    let h = Harness::new().await;
    let (workload, _) = workload(&h).await;
    let (account, bearer) = person(&h, "friend").await;
    let added = profile(&h, &bearer).add_ssh_key(add(1, &account, "laptop")).await.unwrap();
    let key_id = added.into_owned().key.into_option().unwrap().id;
    let created = relay_token(&h, &h.root_org(), "sftp-1").await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let relay_dir = RelayDir::new(dir.path().join("relay"));
    let id = join(&relay_dir, &h.base_url(), &created.token, &created.ca_fingerprint).await;
    let (id, identity) = (id.unwrap(), relay_dir.identity().unwrap());
    let control = relay::RelayControlServiceClient::new(h.http.clone(), h.config(None));
    let mut asked = FilesAsked {
        key: SshPublicKey::parse(&ed25519_possession([1; 32], &account).0).unwrap().blob().to_vec(),
        workload: Id::parse(IdKind::Workload, &workload).unwrap().uuid(),
        client: "192.0.2.7".parse().unwrap(),
        renews: None,
    };
    let ask =
        |a: &FilesAsked| control.authorize_files(signed_files_request(&id, &identity, a).unwrap());

    let none = ask(&asked).await.unwrap_err();
    assert_eq!(none.code, ErrorCode::PermissionDenied, "a workload.read grant opens no files");
    grant(&h, "read", &account, &["workload.file.read"]).await;
    let read = ask(&asked).await.unwrap().into_owned();
    assert_eq!(read.action, "workload.file.read");
    assert!(!read.token.is_empty());
    grant(&h, "write", &account, &["workload.file.write"]).await;
    let write = ask(&asked).await.unwrap().into_owned();
    assert_eq!(write.action, "workload.file.write", "writing where it is allowed");
    asked.renews = Some(write.token);
    ask(&asked).await.unwrap();
    assert_eq!(h.effects("workload.file.sftp_authorized").await.len(), 2, "renewals are not");

    let mut forged = signed_files_request(&id, &identity, &asked).unwrap();
    forged.client = "198.51.100.1".into();
    let forged = control.authorize_files(forged).await.unwrap_err();
    assert_eq!(forged.code, ErrorCode::Unauthenticated, "the signature covers the request");
    let store = h.db.store().await.unwrap();
    let disable =
        |at: &'static str| sqlx::query(at).bind(Id::parse(IdKind::User, &account).unwrap().uuid());
    disable("UPDATE account SET disabled_at = now() WHERE id = $1")
        .execute(store.pool())
        .await
        .unwrap();
    let disabled = ask(&asked).await.unwrap_err();
    assert_eq!(disabled.code, ErrorCode::Unauthenticated, "a disabled account's keys hold nothing");
    disable("UPDATE account SET disabled_at = NULL WHERE id = $1")
        .execute(store.pool())
        .await
        .unwrap();
    let me = profile(&h, &bearer);
    me.remove_ssh_key(pb::RemoveSshKeyRequest { id: key_id, ..Default::default() }).await.unwrap();
    asked.renews = None;
    let gone = ask(&asked).await.unwrap_err();
    assert_eq!(gone.code, ErrorCode::Unauthenticated, "a removed key holds nothing");
}
