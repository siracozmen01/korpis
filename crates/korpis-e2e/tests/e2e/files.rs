// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's files on a real node (§3.2 of docs/06-storage.md), through the control plane's
//! relay to the node's own file worker, its volumes on a btrfs made for the test with qgroups on:
//! the volume is listed and written as the workload sees it; a link the tenant planted is refused
//! rather than followed out; an upload cut off part way is resumed in a new session from the size
//! a stat reports; a write past the volume's quota is refused as the volume being full; and the
//! sessions are on record as their holder's acts. It needs root, containerd, and `mkfs.btrfs`,
//! and is skipped without them.

use crate::node::{Node, live};
use crate::recipes::push_sleeper;
use crate::support::ControlPlane;
use buffa::Message as _;
use korpis_cli::cmd::files::FilesAction;
use korpis_cli::cmd::session::{self, Session};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request as File;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use korpis_proto::korpis::v1::__buffa::oneof::relay_response::Message as Reply;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use std::path::Path;
use std::time::Duration;

const WRITE: &str = "workload.file.write";
const MIB: usize = 1 << 20;

/// A file session on `w`, as whoever `c` signs in as.
pub async fn open(c: &korpis_cli::connect::Connection, w: &str) -> Files {
    let minted = session::mint(c, w, WRITE).await.unwrap();
    let at = session::relay(c, None).await.unwrap();
    Files { session: session::open(&at, w, &minted, Vec::new()).await.unwrap(), next: 0 }
}

pub struct Files {
    session: Session,
    next: u64,
}

impl Files {
    pub async fn ask(&mut self, request: File) -> Response {
        self.next += 1;
        let r = pb::FileRequest { id: self.next, request: Some(request), ..Default::default() };
        self.session.send(session::frame(Frame::Data(r.encode_to_vec()))).await.unwrap();
        let next = self.session.message::<pb::RelayResponse>();
        let got = tokio::time::timeout(Duration::from_secs(30), next).await.unwrap().unwrap();
        let Some(Reply::Frame(f)) = got.map(|m| m.to_owned_message()).and_then(|m| m.message)
        else {
            panic!("no frame")
        };
        let Some(Frame::Data(bytes)) = f.frame else { panic!("not data: {:?}", f.frame) };
        let answer = pb::FileResponse::decode_from_slice(&bytes).unwrap();
        assert_eq!(answer.id, self.next, "answered in order");
        answer.response.unwrap()
    }

    pub async fn write(&mut self, path: &str, offset: u64, data: Vec<u8>) -> Response {
        let w =
            pb::FileWrite { path: path.into(), offset, data, create: true, ..Default::default() };
        self.ask(File::Write(Box::new(w))).await
    }

    async fn size(&mut self, path: &str) -> u64 {
        match self
            .ask(File::Stat(Box::new(pb::FileStat { path: path.into(), ..Default::default() })))
            .await
        {
            Response::Entry(e) => e.size,
            other => panic!("{path}: {other:?}"),
        }
    }
}

fn code(r: &Response) -> Option<pb::FileErrorCode> {
    match r {
        Response::Error(e) => e.code.as_known(),
        _ => None,
    }
}

/// Writes past the volume's 64 MiB, which its quota refuses as full.
async fn fills(f: &mut Files) {
    let mut full = None;
    for i in 0..96u64 {
        let r = f.write("/data/filler", i << 20, vec![1; MIB]).await;
        if code(&r).is_some() {
            full = code(&r);
            break;
        }
    }
    assert_eq!(full, Some(pb::FileErrorCode::FILE_ERROR_CODE_FULL), "the quota refuses");
}

/// Waits until `n` file sessions are on record as ended, each the token's act.
async fn recorded(cp: &ControlPlane, n: i64) {
    let sql = "SELECT count(*) FROM effect WHERE action = 'workload.file.write' \
               AND after->>'event' = 'ended' AND actor::text LIKE '%token%'";
    let mut recorded = 0;
    for _ in 0..300 {
        recorded = cp.scalar::<i64>(sql).await;
        if recorded >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(recorded, n, "every session, as the token's act");
}

#[tokio::test]
async fn a_workload_s_files_are_reached_through_the_relay_confined_resumed_and_recorded() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let Some(node) = Node::join_on_btrfs(&cp, "fra-6").await else { return };
    let agent = node.agent().await;
    let w = cp.declare(&project, "world", &push_sleeper().await).await;
    node.guard(&w);
    let vol = volume(&node, &w).await;
    std::os::unix::fs::symlink("/etc", vol.join("etc")).unwrap();
    let editor = cp.cli(Some(&cp.token_for(&[WRITE]).await));

    let mut f = open(&editor, &w).await;
    let list = pb::FileList { path: "/".into(), limit: 10, ..Default::default() };
    let Response::Entries(top) = f.ask(File::List(Box::new(list))).await else { panic!() };
    let names: Vec<&str> = top.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["data"], "the volume's mount point, and nothing of the host");
    let read = pb::FileRead { path: "/data/etc/passwd".into(), length: 64, ..Default::default() };
    assert_eq!(
        code(&f.ask(File::Read(Box::new(read))).await),
        Some(pb::FileErrorCode::FILE_ERROR_CODE_OUTSIDE)
    );

    // An upload of three mebibytes, cut off after the first, resumed in a new session.
    let world: Vec<u8> = (0..3 * MIB).map(|i| u8::try_from(i % 251).unwrap()).collect();
    assert_eq!(
        f.write("/data/world.zip", 0, world[..MIB].to_vec()).await,
        Response::Written(1 << 20)
    );
    drop(f);
    let mut f = open(&editor, &w).await;
    let mut at = f.size("/data/world.zip").await;
    assert_eq!(at, 1 << 20, "resumed from what was written");
    while let Some(chunk) = world.get(usize::try_from(at).unwrap()..).filter(|c| !c.is_empty()) {
        let chunk = chunk[..chunk.len().min(MIB)].to_vec();
        let Response::Written(n) = f.write("/data/world.zip", at, chunk).await else { panic!() };
        at += n;
    }
    assert_eq!(std::fs::read(vol.join("world.zip")).unwrap(), world);

    fills(&mut f).await;
    drop(f);

    recorded(&cp, 2).await;
    drop(agent);
}

/// The directory of `w`'s volume `data` on `node`, once the node made it.
async fn volume(node: &Node, w: &str) -> std::path::PathBuf {
    let vol = node.volumes.join(format!("{w}.data"));
    for _ in 0..300 {
        if vol.is_dir() {
            return vol;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{} never made", vol.display());
}

/// `korpis files sync` of `local` into `remote`, as `c`.
async fn sync(c: &korpis_cli::connect::Connection, local: &Path, remote: &str) -> String {
    let a = FilesAction::Sync {
        local: local.to_owned(),
        remote: remote.to_owned(),
        resume: false,
        relay: None,
    };
    korpis_files(c, a).await
}

/// Runs `korpis files` `a` as `c`, answering with what it printed for a person.
async fn korpis_files(c: &korpis_cli::connect::Connection, a: FilesAction) -> String {
    korpis_cli::cmd::files::files(c, &a).await.unwrap().human
}

#[tokio::test]
async fn korpis_files_puts_gets_and_syncs_a_world_through_the_relay() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "fra-7").await;
    let agent = node.agent().await;
    let w = cp.declare(&project, "world", &push_sleeper().await).await;
    node.guard(&w);
    let vol = volume(&node, &w).await;
    let editor = cp.cli(Some(&cp.token_for(&[WRITE, "workload.read"]).await));
    let here = tempfile::tempdir().unwrap();
    let world = here.path().join("world");
    std::fs::create_dir_all(world.join("region")).unwrap();
    std::fs::write(world.join("level.dat"), vec![7u8; 3 * MIB + 11]).unwrap();
    std::fs::write(world.join("region/r.0.0.mca"), b"region").unwrap();
    let remote = format!("w:{w}/data/world");

    let first = sync(&editor, &world, &remote).await;
    assert!(first.contains("2 files sent"), "{first}");
    assert_eq!(std::fs::read(vol.join("world/region/r.0.0.mca")).unwrap(), b"region");
    let again = sync(&editor, &world, &remote).await;
    assert!(again.contains("0 files sent") && again.contains("2 already there"), "{again}");

    let listed =
        korpis_files(&editor, FilesAction::Ls { remote: remote.clone(), relay: None }).await;
    assert!(listed.contains("level.dat") && listed.contains("region"), "{listed}");
    let back = here.path().join("level.dat");
    let file = format!("{remote}/level.dat");
    korpis_files(
        &editor,
        FilesAction::Get {
            remote: file.clone(),
            local: Some(back.clone()),
            resume: false,
            relay: None,
        },
    )
    .await;
    assert_eq!(std::fs::read(&back).unwrap(), std::fs::read(world.join("level.dat")).unwrap());

    // A partial upload of the same file continues from what the workload holds.
    std::fs::write(vol.join("world/half.dat"), vec![7u8; MIB]).unwrap();
    let put = korpis_files(
        &editor,
        FilesAction::Put {
            local: world.join("level.dat"),
            remote: format!("{remote}/half.dat"),
            resume: true,
            relay: None,
        },
    )
    .await;
    assert!(put.contains(&format!("{} bytes sent", 2 * MIB + 11)), "{put}");
    korpis_files(&editor, FilesAction::Rm { remote: remote.clone(), recursive: true, relay: None })
        .await;
    assert!(!vol.join("world").exists());
    drop(agent);
}
