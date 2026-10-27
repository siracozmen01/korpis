// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resolving a recipe step through a step provider (§4 of docs/09-recipes.md): a provider written
//! with the SDK answers with content-addressed artifacts, says whether they were pinned, and an
//! answer the node could not safely act on is recorded as the provider's malfunction.

use crate::extensions::{DNS, push_document};
use crate::providers::installed_from;
use crate::support::Harness;
use korpis_app::extension::CallTarget;
use korpis_app::step::StepCall;
use korpis_core::extension::{CallOutcome, Platform, ProviderType};
use korpis_core::value::{Value, ValueMap};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::step::{Failure, Step, StepProvider};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::v1 as pb;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Resolves `appid` 730 to one archive, pinned when a manifest is named; refuses other apps; and
/// answers app 666 with an archive that would escape the volume.
struct Steam;

fn resolve(arguments: &HashMap<String, pb::Value>) -> Result<pb::StepResult, Failure> {
    use korpis_proto::korpis::v1::__buffa::oneof::value::Kind;
    let int = |k: &str| match arguments.get(k).and_then(|v| v.kind.clone()) {
        Some(Kind::IntValue(i)) => Some(i),
        _ => None,
    };
    let path = match int("appid") {
        Some(730) => "game",
        Some(666) => "../../etc",
        _ => {
            let why = "no such app";
            return Err(Failure {
                reason: "steam.app_not_found".into(),
                message: why.into(),
                retryable: false,
            });
        }
    };
    let pinned = int("manifest").is_some();
    let artifact = pb::StepArtifact {
        path: path.into(),
        url: "https://cdn.example/730.tar.zst".into(),
        sha256: "b".repeat(64),
        size: 1024,
        extract: true,
        ..Default::default()
    };
    let resolved =
        if pinned { "manifest 7346914270000188842" } else { "public at 7346914270000188842" };
    Ok(pb::StepResult {
        artifacts: vec![artifact],
        pinned,
        resolved: resolved.into(),
        ..Default::default()
    })
}

impl StepProvider for Steam {
    fn run(
        &self,
        _step: String,
        arguments: HashMap<String, pb::Value>,
        _platform: pb::Platform,
    ) -> impl Future<Output = Result<pb::StepResult, Failure>> + Send {
        std::future::ready(resolve(&arguments))
    }
}

fn arguments(appid: i64, manifest: Option<i64>) -> ValueMap {
    let mut m = ValueMap::new();
    m.insert("appid".into(), Value::Int(appid));
    if let Some(n) = manifest {
        m.insert("manifest".into(), Value::Int(n));
    }
    m
}

/// Installs an extension providing `valve/steam.app` with its argument schema, and serves it with
/// [`Steam`] through the SDK until the test ends; returns the installation.
pub async fn serving(h: &Harness) -> pb::Extension {
    let document = DNS
        .replace(
            "    - {type: dns, name: cloudflare}",
            "    - type: step\n      name: valve/steam.app\n      arguments:\n        \
             - {name: appid, type: int, required: true}\n        - {name: manifest, type: int}",
        )
        .replace(
            "requires: [provider.serve, provider.dns,",
            "requires: [provider.serve, provider.step,",
        );
    let x = installed_from(h, 61, "steps", push_document(&document).await).await;
    let declared = x.extension.manifest.as_option().unwrap().provides[0].arguments.clone();
    let names: Vec<_> = declared.iter().map(|a| (a.name.as_str(), a.r#type.as_str())).collect();
    assert_eq!(names, [("appid", "int"), ("manifest", "int")], "its schema is read back");
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
        relay: None,
    })));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_STEP,
        name: "valve/steam.app".into(),
        version: "1.0.0".into(),
        concurrency: 4,
        sign_in: false,
        fencing: pb::Fencing::FENCING_UNSPECIFIED,
    };
    let provider = Arc::new(Provider::new(connection, registration, Step(Steam)));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    registered.wait_for(Option::is_some).await.unwrap();
    x.extension
}

#[tokio::test]
async fn a_step_resolves_to_content_addressed_artifacts_and_a_bad_answer_never_reaches_a_node() {
    let h = Harness::new().await;
    serving(&h).await;

    let org = korpis_core::id::Id::parse(korpis_core::id::IdKind::Organization, &h.root_org());
    let target = CallTarget {
        organization: org.unwrap().uuid(),
        kind: ProviderType::Step,
        name: "valve/steam.app".into(),
    };
    let run = |args: ValueMap| {
        let step = StepCall {
            workload: uuid::Uuid::now_v7(),
            arguments: args,
            platform: Platform { os: "linux".into(), arch: "amd64".into() },
        };
        let (app, target) = (Arc::clone(&h.api), target.clone());
        async move { app.app().run_step(&target, step, Duration::from_secs(10)).await.unwrap() }
    };

    let CallOutcome::StepResult(pinned) =
        run(arguments(730, Some(7_346_914_270_000_188_842))).await.outcome
    else {
        panic!("a result");
    };
    assert!(pinned.pinned && pinned.artifacts.len() == 1 && pinned.artifacts[0].extract);
    let CallOutcome::StepResult(current) = run(arguments(730, None)).await.outcome else {
        panic!("a result");
    };
    assert!(!current.pinned, "a channel is not pinned: {}", current.resolved);

    let CallOutcome::Failed(f) = run(arguments(1, None)).await.outcome else { panic!("refused") };
    assert_eq!((f.reason.as_str(), f.retryable), ("steam.app_not_found", false));
    let CallOutcome::Failed(f) = run(arguments(666, None)).await.outcome else { panic!("refused") };
    assert_eq!(f.reason, "provider.malformed", "a path out of the volume never reaches a node");
}
