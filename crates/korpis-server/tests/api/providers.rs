// SPDX-License-Identifier: AGPL-3.0-or-later

//! Provider registration (§3.3 of docs/16-extensions.md): an installed extension's workload,
//! authenticated as its installation, opens a stream and registers one of the `provides` entries
//! its installation recorded. The registration lasts as long as the stream and the installation.

use crate::agent::{self, TestAgent};
use crate::extensions::{install, needs, push_dns};
use crate::identity::{ask, connect, send};
use crate::recipes::project;
use crate::support::{Harness, detail, reason};
use connectrpc::client::ClientConfig;
use connectrpc::client::{BidiStream, ClientTransport, HttpClient};
use connectrpc::{ConnectError, ErrorCode};
use korpis_proto::korpis::agent::v1 as wire_agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as AgentUp;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as AgentDown;
use korpis_proto::korpis::agent::v1::__buffa::oneof::workload_credential::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_control::Message as Down;
use korpis_proto::korpis::v1::__buffa::oneof::provider_message::Message as Up;
use std::time::Duration;

pub type Serve = BidiStream<
    <HttpClient as ClientTransport>::ResponseBody,
    pb::ProviderMessage,
    pb::__buffa::view::ProviderControlView<'static>,
>;

/// An installed DNS extension, its node's session kept open, and its workload's credential.
pub struct Installed {
    pub extension: pb::Extension,
    pub secret: String,
    _session: agent::Stream,
}

pub async fn installed(h: &Harness, n: u8, project_name: &str) -> Installed {
    installed_from(h, n, project_name, push_dns().await).await
}

/// An extension pushed to `repo`, installed, and its workload's credential obtained.
pub async fn installed_from(h: &Harness, n: u8, project_name: &str, repo: String) -> Installed {
    let a = TestAgent::enroll(h, &format!("fra-{n}"), n).await;
    let mut session = connect(h, &a, n).await;
    let project = project(h, &h.root_org(), project_name).await;
    let approved = needs(h, &repo, &project).await;
    let req = pb::InstallExtensionRequest {
        approved_needs: approved,
        ..install(h, &format!("install-{n}"), &repo, &project)
    };
    let done = h.extensions(h.root()).install_extension(req).await.unwrap().into_owned();
    let extension = done.extension.into_option().unwrap();
    held(&mut session, &extension.workload, n).await;
    let secret = credential(&mut session, &extension.workload).await;
    Installed { extension, secret, _session: session }
}

/// `workload`'s credential, asked again after every retryable refusal as an agent asks: on a
/// loaded machine the store may answer `credential.unavailable` for a while.
async fn credential(session: &mut agent::Stream, workload: &str) -> String {
    let mut last = String::new();
    for attempt in 0..40u8 {
        match ask(session, workload, &format!("c{attempt}")).await {
            Outcome::Credential(issued) => return issued.secret.clone(),
            Outcome::Refused(e) if e.retryable => last.clone_from(&e.reason),
            Outcome::Refused(e) => panic!("the workload's credential was refused: {}", e.reason),
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!("an installed extension's workload obtains its credential; last refused {last}");
}

/// Asks for `workload`'s lease until it is granted, as an agent does: the credential is asked
/// once the lease is held, and a request that arrives before the workload is placed is refused.
async fn held(session: &mut agent::Stream, workload: &str, n: u8) {
    for attempt in 0..20u8 {
        let lease = wire_agent::LeaseRequest {
            workload: workload.to_owned(),
            nonce: vec![n, attempt, 0, 0, 0, 0, 0, 0],
            ..Default::default()
        };
        send(session, AgentUp::LeaseRequest(Box::new(lease))).await;
        loop {
            match crate::agent::next(session).await.message {
                Some(AgentDown::LeaseGrant(g)) if g.workload == workload => return,
                Some(AgentDown::LeaseRefusal(r)) if r.workload == workload => break,
                _ => {}
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("the lease was never granted");
}

pub fn register(name: &str, version: &str, concurrency: u32) -> pb::ProviderMessage {
    let r = pb::Register {
        r#type: pb::ProviderType::PROVIDER_TYPE_DNS.into(),
        name: name.to_owned(),
        version: version.to_owned(),
        concurrency,
        ..Default::default()
    };
    pb::ProviderMessage { message: Some(Up::Register(Box::new(r))), ..Default::default() }
}

/// Opens a stream as `as_` and sends `first`; the stream and the first answer.
pub async fn open(
    h: &Harness,
    as_: ClientConfig,
    first: pb::ProviderMessage,
) -> (Serve, Result<pb::ProviderControl, ConnectError>) {
    let client = pb::ProviderServiceClient::new(h.http.clone(), as_);
    let mut stream = client.serve().await.unwrap();
    stream.send(first).await.unwrap();
    let first = match stream.message::<pb::ProviderControl>().await {
        Ok(Some(m)) => Ok(m.to_owned_message()),
        Ok(None) => Err(ConnectError::internal("the stream ended")),
        Err(e) => Err(e),
    };
    (stream, first)
}

/// Opens a stream as `as_` and registers `name` at version 1.0.0.
async fn serve(
    h: &Harness,
    as_: ClientConfig,
    name: &str,
) -> (Serve, Result<pb::ProviderControl, ConnectError>) {
    open(h, as_, register(name, "1.0.0", 4)).await
}

async fn registrations(h: &Harness, extension: &str) -> i64 {
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Extension, extension).unwrap();
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar("SELECT count(*) FROM provider_registration WHERE extension_id = $1")
        .bind(id.uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn an_installed_extension_registers_what_it_provides_for_as_long_as_its_stream() {
    let h = Harness::new().await;
    let x = installed(&h, 11, "dns").await;
    let (stream, first) = serve(&h, h.config(Some(&x.secret)), "cloudflare").await;
    let Some(Down::Registered(r)) = first.unwrap().message else { panic!("a Registered answer") };
    assert!(r.registration.starts_with("prv_"), "{}", r.registration);
    assert_eq!(registrations(&h, &x.extension.id).await, 1);
    assert_eq!(h.effects("provider.register").await.len(), 1);

    drop(stream);
    for _ in 0..50 {
        if registrations(&h, &x.extension.id).await == 0 {
            assert_eq!(h.effects("provider.withdraw").await.len(), 1);
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("a closed stream's registration is withdrawn");
}

#[tokio::test]
async fn a_stream_registers_only_what_its_installation_provides_and_only_as_an_extension() {
    let h = Harness::new().await;
    let x = installed(&h, 12, "dns").await;
    let (_s, first) = serve(&h, h.config(Some(&x.secret)), "route53").await;
    let e = first.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert_eq!(detail(&e).field, "register.name");

    let (_s, first) = serve(&h, h.root(), "cloudflare").await;
    assert_eq!(first.unwrap_err().code, ErrorCode::PermissionDenied, "an operator is no provider");
    assert_eq!(registrations(&h, &x.extension.id).await, 0);
}

#[tokio::test]
async fn a_provider_name_is_served_by_one_installation_per_organization() {
    let h = Harness::new().await;
    let _first = installed(&h, 13, "dns").await;
    let repo = push_dns().await;
    let project = project(&h, &h.root_org(), "dns-again").await;
    // Into a project this time, so it is a different installation of the same document.
    let within = |key: &str| pb::InstallExtensionRequest {
        project: Some(project.clone()),
        ..install(&h, key, &repo, &project)
    };
    let dry = pb::InstallExtensionRequest { dry_run: true, ..within("dry") };
    let shown = h.extensions(h.root()).install_extension(dry).await.unwrap().into_owned();
    let approved = shown.extension.into_option().unwrap().manifest.into_option().unwrap().needs;
    let req = pb::InstallExtensionRequest { approved_needs: approved, ..within("again") };
    let e = h.extensions(h.root()).install_extension(req).await.unwrap_err();
    assert_eq!(reason(&e), "provider.taken");
}

#[tokio::test]
async fn uninstalling_withdraws_the_registrations_and_ends_their_streams() {
    let h = Harness::new().await;
    let x = installed(&h, 14, "dns").await;
    let (mut stream, first) = serve(&h, h.config(Some(&x.secret)), "cloudflare").await;
    assert!(matches!(first.unwrap().message, Some(Down::Registered(_))));
    let req = pb::UninstallExtensionRequest {
        mutation: crate::support::mutation("u").into(),
        id: x.extension.id.clone(),
        ..Default::default()
    };
    h.extensions(h.root()).uninstall_extension(req).await.unwrap();
    assert_eq!(registrations(&h, &x.extension.id).await, 0);
    let ended =
        tokio::time::timeout(Duration::from_secs(15), stream.message::<pb::ProviderControl>())
            .await
            .expect("the stream ends within a few confirmations");
    assert!(matches!(ended, Ok(None) | Err(_)), "the stream of a withdrawn registration ends");
}
