// SPDX-License-Identifier: AGPL-3.0-or-later

//! A recipe's install on a real node (§4 of docs/09-recipes.md, §31 of docs/23-walkthroughs.md).
//! Every verb but `chown` runs beneath the workload's volume, as the workload's own root, in the
//! helper's confined worker: a fetch served on loopback, an archive the recipe carries extracted,
//! a file written, a mode set, a link made, a directory made, and a template rendered. An install
//! that fails at its extract for want of room resumes there once the volume is declared larger,
//! its earlier actions not run again, and a new version of the recipe installs again.

use crate::node::Node;
use crate::recipes::busybox;
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use korpis_recipe::artifact::{Artifact, File};
use korpis_recipe::registry::Registry;
use sha2::Digest as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

pub const JAR: &[u8] = b"not really a jar\n";
pub const MIB: u64 = 1 << 20;

/// Serves [`JAR`] on loopback, counting the requests; its URL and the count.
pub async fn serve_jar() -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/paper.jar", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&count);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut request = [0u8; 4096];
                let _ = s.read(&mut request).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    JAR.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(JAR).await;
            });
        }
    });
    (url, count)
}

/// A tar of `pkg/` holding a script and a world of `world` bytes.
fn archive(world: u64) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let mut add = |path: &str, data: &[u8]| {
        let mut h = tar::Header::new_gnu();
        h.set_size(u64::try_from(data.len()).unwrap());
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, path, data).unwrap();
    };
    add("pkg/run.sh", b"#!/bin/sh\necho ok\n");
    add("pkg/world.dat", &vec![7u8; usize::try_from(world).unwrap()]);
    b.into_inner().unwrap()
}

/// The installing recipe at `version` for `tier`, fetching the jar from `url`, writing `eula`.
pub fn document(tier: &str, version: &str, url: &str, eula: &str) -> String {
    let busybox = busybox();
    let sha = sha2::Sha256::digest(JAR).iter().fold(String::new(), |s, b| s + &format!("{b:02x}"));
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: installer, version: "{version}", description: installs, license: MIT}}
runtime:
  targets:
    - {{tier: {tier}, arch: [amd64, arm64], image: "{busybox}"}}
  entry: {{command: [/bin/sh, -c, "exec sleep 3600"]}}
preset:
  lifecycle: persistent
  console: none
  volumes:
    - {{name: data, path: /data, size_default: 16Mi}}
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu: {{min: 100m, recommended: 250m}}
config:
  fields:
    - {{key: motd, type: string, default: hi, pattern: '[^\r\n]*'}}
install:
  steps:
    - fetch: {{url: "{url}", sha256: "{sha}", to: /data/server.jar}}
    - write: {{path: /data/eula.txt, content: "{eula}"}}
    - extract: {{from: files/pkg.tar, to: /data/pkg, strip: 1}}
    - chmod: {{path: /data/pkg/run.sh, mode: "0755"}}
    - symlink: {{target: pkg/run.sh, link: /data/run}}
    - mkdir: {{path: /data/plugins, mode: "0755"}}
templates:
  - {{from: templates/server.properties, path: /data/server.properties, format: properties, vars: [motd]}}
"#
    )
}

/// Pushes `document` with its files to `repo` as `version`.
pub async fn push(repo: &str, version: &str, document: String) {
    let file = |path: &str, data: Vec<u8>| File {
        path: path.to_owned(),
        media_type: "application/octet-stream".to_owned(),
        data,
    };
    let artifact = Artifact {
        document: document.into_bytes(),
        files: vec![
            file("files/pkg.tar", archive(32 * MIB)),
            file("templates/server.properties", b"motd={{motd}}\n".to_vec()),
        ],
    };
    let registry = repo.split('/').next().unwrap().to_owned();
    Registry::new(vec![registry]).unwrap().push(repo, version, &artifact).await.unwrap();
}

/// A declaration of `version` of the recipe at `repo`, its volume `size` bytes: new as `name` in
/// `project`, or for `workload` at intent `at`.
pub fn declaration(
    (repo, version): (&str, &str),
    size: u64,
    target: Target,
    (key, at): (&str, Option<u64>),
) -> pb::DeclareIntentRequest {
    let volume = pb::VolumeSpec {
        name: "data".into(),
        path: "/data".into(),
        size_bytes: size,
        ..Default::default()
    };
    pb::DeclareIntentRequest {
        mutation: pb::Mutation {
            idempotency_key: key.into(),
            expected_version: at,
            ..Default::default()
        }
        .into(),
        target: Some(target),
        body: pb::IntentBody {
            recipe: pb::RecipeRef {
                name: repo.into(),
                version: version.into(),
                ..Default::default()
            }
            .into(),
            volumes: vec![volume],
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// Declares, approves the plan if it waits for a person, and waits for it to become `wanted`; the
/// workload and the plan.
pub async fn declared(
    cp: &ControlPlane,
    d: pb::DeclareIntentRequest,
    wanted: pb::PlanStatus,
) -> (String, pb::Plan) {
    let out = Box::pin(cp.workloads().declare_intent(d)).await.unwrap().into_owned();
    let workload = out.workload.as_option().unwrap().id.clone();
    let plan = out.plan.into_option().unwrap();
    if plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
        cp.approve(&plan.id).await;
    }
    let plans = pb::PlanServiceClient::new(cp.http(), cp.config());
    for _ in 0..900 {
        let got = plans.get_plan(pb::GetPlanRequest { id: plan.id.clone(), ..Default::default() });
        let got = got.await.unwrap().into_owned().plan.into_option().unwrap();
        if got.status.as_known() == Some(wanted) {
            return (workload, got);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let said = cp.scalar::<Option<String>>(EFFECTS).await;
    panic!("plan {} never became {wanted:?}; what the node said: {said:?}", plan.id);
}

/// What each step's effect said, for a plan that never ended as awaited.
const EFFECTS: &str = "SELECT string_agg(action || ' ' || outcome || ' ' || coalesce(error::text, ''),                        '; ' ORDER BY at) FROM effect WHERE action LIKE 'step.%'";

#[tokio::test]
async fn a_recipe_installs_resumes_where_it_failed_and_installs_again_when_upgraded() {
    if !crate::node::live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let Some(node) = Node::join_on_btrfs(&cp, "fra-install").await else { return };
    let _agent = node.agent().await;
    let project = cp.project().await;
    let (url, fetches) = serve_jar().await;
    let registry =
        std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned());
    let repo = format!("{registry}/test/installer-{}", uuid::Uuid::now_v7().simple());
    push(&repo, "1.0.0", document("container", "1.0.0", &url, "eula=true\\n")).await;

    // Sixteen MiB will not hold a thirty-two MiB world: the extract fails, the two before it done.
    let new = Target::Create(Box::new(pb::NewWorkload {
        project: project.clone(),
        name: "mc".into(),
        ..Default::default()
    }));
    let first = declaration((&repo, "1.0.0"), 16 * MIB, new, ("d1", None));
    let (w, failed) = declared(&cp, first, pb::PlanStatus::PLAN_STATUS_FAILED).await;
    node.guard(&w);
    let vol = node.volumes.join(format!("{w}.data"));
    assert_eq!(std::fs::read(vol.join("server.jar")).unwrap(), JAR, "fetched and written");
    let eula = vol.join("eula.txt");
    let written = std::fs::metadata(&eula).unwrap().modified().unwrap();
    assert!(failed.completed.is_some(), "the steps before the install completed");

    // Larger now: the next plan resumes the install at its extract, and the workload runs.
    let again =
        declaration((&repo, "1.0.0"), 96 * MIB, Target::Workload(w.clone()), ("d2", Some(1)));
    declared(&cp, again, pb::PlanStatus::PLAN_STATUS_APPLIED).await;
    assert_eq!(std::fs::metadata(&eula).unwrap().modified().unwrap(), written, "not written again");
    assert_eq!(fetches.load(Ordering::SeqCst), 1, "fetched once: the node keeps it by its hash");
    let world = std::fs::metadata(vol.join("pkg/world.dat")).unwrap();
    assert_eq!(world.len(), 32 * MIB, "extracted whole, its top directory stripped");
    // Stored by the ids the workload sees, its own root here; the mapping is the mount's (§4.5 of
    // docs/04-runtimes.md).
    assert_eq!((world.uid(), world.gid()), (0, 0), "the workload's own root, as stored");
    let run = std::fs::metadata(vol.join("pkg/run.sh")).unwrap();
    assert_eq!(run.permissions().mode() & 0o777, 0o755);
    assert_eq!(std::fs::read_link(vol.join("run")).unwrap(), std::path::Path::new("pkg/run.sh"));
    assert!(std::fs::metadata(vol.join("plugins")).unwrap().is_dir());
    assert_eq!(std::fs::read_to_string(vol.join("server.properties")).unwrap(), "motd=hi\n");

    // A new version of the recipe: stopped, installed again, started.
    push(&repo, "2.0.0", document("container", "2.0.0", &url, "eula=true\\n# v2\\n")).await;
    let upgrade =
        declaration((&repo, "2.0.0"), 96 * MIB, Target::Workload(w.clone()), ("d3", Some(2)));
    let (_, plan) = declared(&cp, upgrade, pb::PlanStatus::PLAN_STATUS_APPLIED).await;
    let steps: Vec<_> = plan.steps.iter().map(|s| s.action.as_str()).collect();
    assert!(steps.contains(&"install"), "{steps:?}");
    assert_eq!(std::fs::read_to_string(&eula).unwrap(), "eula=true\n# v2\n");
    cp.retire(&w).await;
}
