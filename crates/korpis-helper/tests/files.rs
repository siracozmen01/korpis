// SPDX-License-Identifier: AGPL-3.0-or-later

//! File sessions through the helper's verb, with the real worker, on a real ext4 with project
//! quota: the workload's volumes, one mounted inside the other, served at their paths and nothing
//! else; what is written is the workload's, given to the owner of the directory it is made in; a
//! link a tenant planted refused rather than followed out; a volume the workload mounts read-only
//! only read; a write past the volume's quota refused as the volume being full; a session opened
//! to read changing nothing; and a request naming another workload's ids refused. It needs root
//! and `mkfs.ext4`, and is skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

#[allow(dead_code, reason = "each test binary uses part of the shared fixture")]
mod fixture;
#[allow(dead_code, reason = "each test binary uses part of the shared fixture")]
#[path = "fixture/jobs.rs"]
mod jobs;

use buffa::Message as _;
use jobs::{ask, files_binary};
use korpis_core::workload::assignment::IdRange;
use korpis_helper::files::{Files, FilesConfig};
use korpis_helper::helper::Helper;
use korpis_helper::ids::Ledger;
use korpis_helper::storage::Class;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// The id range `wl_1` holds.
fn range() -> IdRange {
    IdRange::of_slot(7).unwrap()
}

/// A helper opening file sessions over `wl_1.data`, of 1 MiB, and `wl_1.plugins`, on the
/// filesystem at `at`; each volume's directory.
fn setup(at: &Path, state: &Path) -> (Helper, PathBuf, PathBuf) {
    let mut class = Class::open("local", &at.join("volumes")).unwrap();
    let mut dir = |volume: &str, size_bytes: u64| {
        let r = helper::EnsureVolumeRequest {
            volume: volume.into(),
            workload: "wl_1".into(),
            size_bytes,
            inodes: 1000,
            ..Default::default()
        };
        PathBuf::from(class.ensure(&r).unwrap().host_path)
    };
    let (data, plugins) = (dir("wl_1.data", 1 << 20), dir("wl_1.plugins", 1 << 30));
    let mut ledger = Ledger::open(state).unwrap();
    ledger.ensure("wl_1", range()).unwrap();
    ledger.ensure("wl_2", IdRange::of_slot(8).unwrap()).unwrap();
    let config = FilesConfig { binary: files_binary(), state: state.to_owned(), cgroups: None };
    let h = Helper::new(vec![class], ledger).with_files(Files::new(config));
    (h, data, plugins)
}

/// Opens a session on `wl_1`'s volumes, as `id_base`, for writing when `write`.
fn open(h: &mut Helper, id_base: u32, write: bool) -> Result<Session, helper::HelperError> {
    let volume = |path: &str, volume: &str| helper::FileVolume {
        path: path.into(),
        volume: volume.into(),
        ..Default::default()
    };
    let r = helper::OpenFilesRequest {
        workload: "wl_1".into(),
        id_base,
        volumes: vec![
            volume("/data", "wl_1.data"),
            volume("/data/plugins", "wl_1.plugins"),
            // The same volume again where the workload mounts it read-only.
            helper::FileVolume { read_only: true, ..volume("/conf", "wl_1.plugins") },
        ],
        write,
        ..Default::default()
    };
    match ask(h, Verb::OpenFiles(Box::new(r))) {
        Answer::OpenFiles(a) => {
            let mut fds = h.take_attached();
            assert_eq!((a.attached, fds.len()), (1, 1));
            Ok(Session { stream: UnixStream::from(fds.remove(0)), next: 0 })
        }
        Answer::Error(e) => Err(*e),
        other => panic!("{other:?}"),
    }
}

struct Session {
    stream: UnixStream,
    next: u64,
}

impl Session {
    fn ask(&mut self, request: Request) -> Response {
        self.next += 1;
        let r = pb::FileRequest { id: self.next, request: Some(request), ..Default::default() };
        korpis_files::frame::write(&mut self.stream, &r.encode_to_vec()).unwrap();
        let bytes = korpis_files::frame::read(&mut self.stream).unwrap().unwrap();
        let answer = pb::FileResponse::decode_from_slice(&bytes).unwrap();
        assert_eq!(answer.id, self.next, "answered in order");
        answer.response.unwrap()
    }

    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Response {
        self.ask(Request::Write(Box::new(pb::FileWrite {
            path: path.into(),
            offset,
            data: data.to_vec(),
            create: true,
            ..Default::default()
        })))
    }

    fn read(&mut self, path: &str) -> Response {
        let r = pb::FileRead { path: path.into(), length: 1 << 20, ..Default::default() };
        self.ask(Request::Read(Box::new(r)))
    }

    fn names(&mut self, path: &str) -> Vec<String> {
        let r = pb::FileList { path: path.into(), limit: 100, ..Default::default() };
        match self.ask(Request::List(Box::new(r))) {
            Response::Entries(e) => e.entries.iter().map(|e| e.name.clone()).collect(),
            other => panic!("{path}: {other:?}"),
        }
    }
}

fn refused(r: &Response) -> pb::FileErrorCode {
    match r {
        Response::Error(e) => e.code.as_known().unwrap(),
        other => panic!("not refused: {other:?}"),
    }
}

/// Writes past the 1 MiB quota of `/data`, which refuses it as full.
fn fills_the_volume(s: &mut Session) {
    let full = s.write("/data/big", 0, &vec![7; 1 << 20]);
    let more = s.write("/data/big", 1 << 20, &vec![7; 1 << 20]);
    let code = |r: &Response| match r {
        Response::Error(e) => e.code.as_known(),
        _ => None,
    };
    assert!(
        [code(&full), code(&more)].contains(&Some(pb::FileErrorCode::FILE_ERROR_CODE_FULL)),
        "the volume's quota refuses: {full:?} {more:?}"
    );
}

#[test]
fn a_file_session_serves_the_workload_s_volumes_and_nothing_else() {
    let root = rustix::process::geteuid().is_root();
    let tmp = tempfile::tempdir().unwrap();
    let Some(m) = root.then(|| fixture::ext4(tmp.path())).flatten() else {
        eprintln!("file sessions need root and mkfs.ext4: not run");
        return;
    };
    let (mut h, data, plugins) = setup(&m.0, &tmp.path().join("state"));
    std::fs::write(plugins.join("a.jar"), b"jar").unwrap();
    std::fs::create_dir(data.join("home")).unwrap();
    std::os::unix::fs::chown(data.join("home"), Some(1000), Some(1000)).unwrap();
    std::os::unix::fs::symlink("/etc", data.join("etc")).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", data.join("hostname")).unwrap();
    let mut s = open(&mut h, range().base, true).unwrap();

    assert_eq!(s.names("/"), ["conf", "data"], "the root holds the volumes' mount points alone");
    assert_eq!(s.read("/conf/a.jar"), Response::Data(b"jar".to_vec()));
    let ro = s.write("/conf/b.jar", 0, b"x");
    assert_eq!(refused(&ro), pb::FileErrorCode::FILE_ERROR_CODE_REFUSED, "mounted read-only");
    assert!(!plugins.join("b.jar").exists());
    assert_eq!(s.names("/data/plugins"), ["a.jar"], "the nested volume, as itself");
    assert_eq!(s.read("/data/plugins/a.jar"), Response::Data(b"jar".to_vec()));
    assert_eq!(s.write("/data/up.txt", 0, b"hello"), Response::Written(5));
    assert_eq!(std::fs::read(data.join("up.txt")).unwrap(), b"hello");
    let owner = |p: &Path| std::fs::symlink_metadata(p).map(|m| (m.uid(), m.gid())).unwrap();
    assert_eq!(owner(&data.join("up.txt")), (0, 0), "the workload's root, as it sees it");
    assert_eq!(s.write("/data/home/x", 0, b"x"), Response::Written(1));
    assert_eq!(owner(&data.join("home/x")), (1000, 1000), "the directory's owner's");

    for path in ["/data/etc/passwd", "/data/hostname", "/etc/passwd", "/proc/self/environ"] {
        let code = refused(&s.read(path));
        assert!(
            [
                pb::FileErrorCode::FILE_ERROR_CODE_OUTSIDE,
                pb::FileErrorCode::FILE_ERROR_CODE_NOT_FOUND
            ]
            .contains(&code),
            "{path}: {code:?}"
        );
    }
    assert_eq!(
        refused(&s.write("/data/plugins/../../x", 0, b"x")),
        pb::FileErrorCode::FILE_ERROR_CODE_OUTSIDE
    );

    fills_the_volume(&mut s);
    drop(s);

    let mut r = open(&mut h, range().base, false).unwrap();
    assert_eq!(r.read("/data/up.txt"), Response::Data(b"hello".to_vec()));
    assert_eq!(
        refused(&r.write("/data/up.txt", 0, b"x")),
        pb::FileErrorCode::FILE_ERROR_CODE_REFUSED
    );
    drop(r);

    let other = open(&mut h, IdRange::of_slot(8).unwrap().base, true).err().unwrap();
    assert_eq!(other.code.as_known(), Some(helper::HelperErrorCode::HELPER_ERROR_CODE_REFUSED));
}
