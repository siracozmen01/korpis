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
use korpis_proto::korpis::agent::v1::__buffa::oneof::workload_credential::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_control::Message as Down;
use korpis_proto::korpis::v1::__buffa::oneof::provider_message::Message as Up;
use std::time::Duration;

type Serve = BidiStream<
    <HttpClient as ClientTransport>::ResponseBody,
    pb::ProviderMessage,
    pb::__buffa::view::ProviderControlView<'static>,
>;

/// An installed DNS extension, its node's session kept open, and its workload's credential.
struct Installed {
    extension: pb::Extension,
    secret: String,
    _session: agent::Stream,
}

async fn installed(h: &Harness, n: u8, project_name: &str) -> Installed {
    let a = TestAgent::enroll(h, &format!("fra-{n}"), n).await;
    let mut session = connect(h, &a, n).await;
    let repo = push_dns().await;
    let project = project(h, &h.root_org(), project_name).await;
    let approved = needs(h, &repo, &project).await;
    let req = pb::InstallExtensionRequest {
        approved_needs: approved,
        ..install(h, "install", &repo, &project)
    };
    let done = h.extensions(h.root()).install_extension(req).await.unwrap().into_owned();
    let extension = done.extension.into_option().unwrap();
    let lease = wire_agent::LeaseRequest {
        workload: extension.workload.clone(),
        nonce: vec![n; 8],
        ..Default::default()
    };
    send(&mut session, AgentUp::LeaseRequest(Box::new(lease))).await;
    let Outcome::Credential(issued) = ask(&mut session, &extension.workload, "c").await else {
        panic!("an installed extension's workload obtains its credential");
    };
    Installed { extension, secret: issued.secret.clone(), _session: session }
}

fn register(name: &str) -> pb::ProviderMessage {
    let r = pb::Register {
        r#type: pb::ProviderType::PROVIDER_TYPE_DNS.into(),
        name: name.to_owned(),
        version: "1.0.0".into(),
        concurrency: 4,
        ..Default::default()
    };
    pb::ProviderMessage { message: Some(Up::Register(Box::new(r))), ..Default::default() }
}

/// Opens a stream as `secret` and registers `name`; the stream and the first answer.
async fn serve(
    h: &Harness,
    as_: ClientConfig,
    name: &str,
) -> (Serve, Result<pb::ProviderControl, ConnectError>) {
    let client = pb::ProviderServiceClient::new(h.http.clone(), as_);
    let mut stream = client.serve().await.unwrap();
    stream.send(register(name)).await.unwrap();
    let first = match stream.message::<pb::ProviderControl>().await {
        Ok(Some(m)) => Ok(m.to_owned_message()),
        Ok(None) => Err(ConnectError::internal("the stream ended")),
        Err(e) => Err(e),
    };
    (stream, first)
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
