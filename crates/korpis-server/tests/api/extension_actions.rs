// SPDX-License-Identifier: AGPL-3.0-or-later

//! An extension's actions: the invoker is authorized for `ext.<name>.<action>` on what they name,
//! the extension is sent the invocation with a token, and with that token it acts on what the
//! invoker named and nothing else, only until the invocation finishes; the invocation is an
//! effect. A resource outside where the extension is installed is refused before it hears.

use crate::extensions::{DNS, push_document};
use crate::providers::{Installed, Serve, installed_from, open};
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, detail};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::action_value::Value as V;
use korpis_proto::korpis::v1::__buffa::oneof::call_result::Outcome;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use korpis_proto::korpis::v1::__buffa::oneof::provider_control::Message as Down;
use korpis_proto::korpis::v1::__buffa::oneof::provider_message::Message as Up;

/// The DNS extension, also needing to read the workloads it is invoked on.
fn reading() -> String {
    DNS.replace(
        "    - actions: [endpoint.read]",
        "    - actions: [workload.read]\n      scope: target\n      reason: to read what it is \
         invoked on\n    - actions: [endpoint.read]",
    )
}

/// Registers the installation's actions on a stream of its own.
async fn serving(h: &Harness, x: &Installed) -> Serve {
    let r = pb::Register {
        r#type: pb::ProviderType::PROVIDER_TYPE_ACTION.into(),
        name: "whatever-the-stream-says".into(),
        version: "1.0.0".into(),
        concurrency: 4,
        ..Default::default()
    };
    let first = pb::ProviderMessage { message: Some(Up::Register(Box::new(r))), ..Default::default() };
    let (stream, answer) = open(h, h.config(Some(&x.secret)), first).await;
    assert!(matches!(answer.unwrap().message, Some(Down::Registered(_))), "registered");
    stream
}

async fn workload(h: &Harness, project: &str, name: &str) -> String {
    let declared = create(name, project, name, body(&push_paper().await, &[]));
    let out = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    out.workload.as_option().unwrap().id.clone()
}

fn purge(extension: &str, workload: &str) -> pb::InvokeActionRequest {
    let value = pb::ActionValue { value: Some(V::Workload(workload.to_owned())), ..Default::default() };
    pb::InvokeActionRequest {
        extension: extension.to_owned(),
        action: "purge".into(),
        arguments: [("workload".to_owned(), value)].into_iter().collect(),
        ..Default::default()
    }
}

/// The next call sent down `stream`: its identifier and the invocation.
async fn invoked(stream: &mut Serve) -> (String, pb::InvokeAction) {
    loop {
        let m = stream.message::<pb::ProviderControl>().await.unwrap().unwrap().to_owned_message();
        if let Some(Down::Call(c)) = m.message
            && let Some(Request::InvokeAction(i)) = c.request
        {
            return (c.call, *i);
        }
    }
}

async fn read(h: &Harness, secret: &str, token: Option<&str>, id: &str) -> Result<(), ErrorCode> {
    let mut config = h.config(Some(secret));
    if let Some(token) = token {
        config = config.with_default_header("korpis-invocation", token.to_owned());
    }
    let client = pb::WorkloadServiceClient::new(h.http.clone(), config);
    let get = pb::GetWorkloadRequest { id: id.to_owned(), ..Default::default() };
    client.get_workload(get).await.map(|_| ()).map_err(|e| e.code)
}

#[tokio::test]
async fn an_action_acts_for_its_invoker_only_on_what_they_named_until_it_finishes() {
    let h = Harness::new().await;
    let x = installed_from(&h, 31, "dns", push_document(&reading()).await).await;
    let mut stream = serving(&h, &x).await;
    let games = project(&h, &h.root_org(), "games").await;
    let (named, other) = (workload(&h, &games, "named").await, workload(&h, &games, "other").await);

    let client = h.extensions(h.root());
    let asked = purge(&x.extension.id, &named);
    let pending = tokio::spawn(async move { client.invoke_action(asked).await });
    let (call, invocation) = invoked(&mut stream).await;
    assert_eq!(invocation.action, "purge");
    let token = invocation.invocation.as_str();
    assert!(token.starts_with("korpisiv_"), "a token, not the invoker's credential");
    assert_eq!(read(&h, &x.secret, Some(token), &named).await, Ok(()));
    assert!(read(&h, &x.secret, Some(token), &other).await.is_err(), "not what was named");
    assert_eq!(read(&h, &x.secret, None, &other).await, Ok(()), "its own grant reaches it");

    let answer = pb::ActionResult { message: "purged".into(), ..Default::default() };
    let result = pb::CallResult { call, outcome: Some(Outcome::ActionResult(Box::new(answer))), ..Default::default() };
    let up = pb::ProviderMessage { message: Some(Up::Result(Box::new(result))), ..Default::default() };
    stream.send(up).await.unwrap();
    let done = pending.await.unwrap().unwrap().into_owned();
    assert_eq!(done.result.as_option().unwrap().message, "purged");
    assert!(done.invocation.starts_with("inv_"));
    let after = read(&h, &x.secret, Some(token), &named).await;
    assert_eq!(after, Err(ErrorCode::Unauthenticated), "the token dies with its invocation");
    assert_eq!(h.effects("extension.invoke").await.len(), 1);
}

#[tokio::test]
async fn an_object_outside_where_the_extension_is_installed_is_refused_before_it_hears() {
    let h = Harness::new().await;
    let repo = push_document(&reading()).await;
    let inside = project(&h, &h.root_org(), "inside").await;
    let x = crate::providers::installed_into(&h, 32, &inside, repo).await;
    let outside = project(&h, &h.root_org(), "outside").await;
    let elsewhere = workload(&h, &outside, "elsewhere").await;
    let e = h.extensions(h.root()).invoke_action(purge(&x.extension.id, &elsewhere)).await;
    let e = e.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert_eq!(detail(&e).field, "arguments[0]");
}

#[tokio::test]
async fn an_action_not_declared_or_given_wrong_arguments_is_refused() {
    let h = Harness::new().await;
    let x = installed_from(&h, 33, "dns", push_document(&reading()).await).await;
    let games = project(&h, &h.root_org(), "games").await;
    let named = workload(&h, &games, "named").await;
    let mut kick = purge(&x.extension.id, &named);
    kick.action = "kick".into();
    let e = h.extensions(h.root()).invoke_action(kick).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    let mut wrong = purge(&x.extension.id, &named);
    let text = pb::ActionValue { value: Some(V::Text("x".into())), ..Default::default() };
    wrong.arguments.insert("workload".into(), text);
    let e = h.extensions(h.root()).invoke_action(wrong).await.unwrap_err();
    assert_eq!(detail(&e).field, "arguments.workload");
}
