// SPDX-License-Identifier: AGPL-3.0-or-later

//! A microVM booted as its driver will boot one (§6 of docs/04-runtimes.md): the node's guest
//! kernel (scripts/guest-kernel.sh), a boot drive holding nothing but `korpis-guest`, an image
//! drive built from an image's layer by `korpis-image-build`, the config drive, a volume, and a
//! vsock device. The entry runs as its user with its environment, writes its volume, and its exit
//! status comes back on the config drive; a stop sent as Ctrl+Alt+Del reaches it as `SIGTERM`;
//! and the guest's agent answers on its control port and sends the entry a signal named. Needs root, /dev/kvm, Firecracker (scripts/tools.sh), the guest
//! kernel, and mkfs.ext4 and debugfs, and is skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_guest::config::{Config, Volume};
use korpis_guest::control::{self, Exec, Exited, Kind, Pong, Resized, Signalled};
use korpis_guest::status::Ended;
use korpis_image::build::Request;
use korpis_image::source::Packing;
use korpis_image::testing::Entry;
use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Firecracker and the guest kernel, if this machine can boot a guest.
fn machine() -> Option<(PathBuf, PathBuf)> {
    let tools = repo().join(".tools");
    let (firecracker, kernel) = (tools.join("bin/firecracker"), tools.join("guest/vmlinux"));
    let root = nix_root();
    let kvm = std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok();
    (root && kvm && firecracker.exists() && kernel.exists()).then_some((firecracker, kernel))
}

fn nix_root() -> bool {
    std::fs::metadata("/proc/self").is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0)
}

fn ok(c: &mut Command) {
    let out = c.output().unwrap();
    assert!(out.status.success(), "{c:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// Where this test was built.
fn target() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().and_then(Path::parent).and_then(Path::parent).unwrap().to_owned()
}

/// `korpis-guest`, built static where this test was built.
fn musl(package: &str, bin: &str) -> PathBuf {
    let triple = "x86_64-unknown-linux-musl";
    ok(Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", package, "--bin", bin, "--target", triple])
        .arg("--target-dir")
        .arg(target()));
    target().join(triple).join("debug").join(bin)
}

/// The image drive a driver would build for an image of busybox and its applets: a layer
/// from the content store, applied and made a filesystem by `korpis-image-build`.
fn image_drive(dir: &Path, out: &Path) {
    ok(Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", "korpis-image", "--bin", "korpis-image-build"])
        .arg("--target-dir")
        .arg(target()));
    let busybox = std::fs::read("/bin/busybox").unwrap();
    let applets = ["sh", "echo", "id", "sleep", "cat", "pwd", "touch"].map(|a| format!("bin/{a}"));
    let mut entries = vec![Entry::File("bin/busybox", &busybox, (0, 0), 0o755)];
    entries.extend(applets.iter().map(|a| Entry::Symlink(a, "busybox")));
    let content = dir.join("content");
    let image = korpis_image::testing::image(
        &content,
        &[(korpis_image::testing::layer(&entries).unwrap(), Packing::Gzip)],
        "1000:1000",
    )
    .unwrap();
    let request = Request { content, image, out: out.to_owned(), ids: 1_900_065_536 };
    korpis_image::build::run(&target().join("debug/korpis-image-build"), &request).unwrap();
}

/// An ext4 image of `size` MiB at `image` holding `dir`'s tree, owned as `owner` at its root.
fn ext4(dir: &Path, image: &Path, size: u32, owner: &str) {
    ok(Command::new("mkfs.ext4")
        .args(["-q", "-F", "-E", &format!("root_owner={owner}"), "-d"])
        .arg(dir)
        .arg(image)
        .arg(format!("{size}M")));
}

/// The four drives of a guest whose entry is `script`, in `dir`.
fn drives(dir: &Path, script: &str) -> [PathBuf; 4] {
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let (guest, fsworker, install_job) = (
        musl("korpis-guest", "korpis-guest"),
        musl("korpis-files", "korpis-fsworker"),
        musl("korpis-install", "korpis-install-job"),
    );
    let programs = korpis_image::boot::Programs {
        guest: &guest,
        fsworker: &fsworker,
        install_job: &install_job,
    };
    let boot = korpis_image::boot::drive(&programs, dir).unwrap();
    let paths = [boot, dir.join("image.ext4"), dir.join("config"), dir.join("data.ext4")];
    image_drive(dir, &paths[1]);
    ext4(&empty, &paths[3], 16, "1000:1000");
    let config = Config {
        hostname: "guest-1".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        env: BTreeMap::from([
            ("PATH".into(), "/bin".into()),
            ("GREETING".into(), "hello from the guest".into()),
        ]),
        working_dir: "/data".into(),
        uid: 1000,
        gid: 1000,
        network: None,
        volumes: vec![Volume { path: "/data".into(), read_only: false }],
        interactive: false,
        identity: false,
        role: korpis_guest::config::Role::Workload,
    };
    config.write(&mut std::fs::File::create(&paths[2]).unwrap()).unwrap();
    paths
}

/// Firecracker's configuration for `drives` and `kernel`, its vsock device's socket at `vsock`.
fn vm(kernel: &Path, drives: &[PathBuf; 4], vsock: &Path) -> String {
    let drive = |id: &str, path: &Path, root: bool, ro: bool| {
        serde_json::json!({"drive_id": id, "path_on_host": path, "is_root_device": root,
                           "is_read_only": ro})
    };
    serde_json::json!({
        "boot-source": {
            "kernel_image_path": kernel,
            "boot_args": "console=ttyS0 reboot=k panic=1 pci=off quiet root=/dev/vda ro \
                          init=/korpis-guest",
        },
        "drives": [
            drive("boot", &drives[0], true, true),
            drive("image", &drives[1], false, true),
            drive("config", &drives[2], false, false),
            drive("data", &drives[3], false, false),
        ],
        "machine-config": {"vcpu_count": 1, "mem_size_mib": 128},
        "vsock": {"guest_cid": control::GUEST_CID, "uds_path": vsock},
    })
    .to_string()
}

/// A guest booted with `script` as its entry, its API on `api`: the process, its drives.
fn boot(dir: &Path, script: &str, api: &Path) -> (std::process::Child, [PathBuf; 4]) {
    let (firecracker, kernel) = machine().unwrap();
    let drives = drives(dir, script);
    let config = dir.join("vm.json");
    std::fs::write(&config, vm(&kernel, &drives, &dir.join("vsock.sock"))).unwrap();
    let child = Command::new(firecracker)
        .arg("--api-sock")
        .arg(api)
        .arg("--config-file")
        .arg(&config)
        .args(["--level", "Error"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    (child, drives)
}

/// What the guest printed until it exited, which it must within `limit`.
fn console(child: &mut std::process::Child, limit: Duration) -> String {
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        assert!(start.elapsed() < limit, "the guest did not end within {limit:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    out
}

fn ended(config: &Path) -> Option<Ended> {
    Ended::read(&mut std::fs::File::open(config).unwrap()).unwrap()
}

#[test]
fn the_entry_runs_as_its_user_writes_its_volume_and_its_exit_status_comes_back() {
    if machine().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let script = "echo $GREETING; id -u; pwd; echo kept > /data/kept; \
                  touch /etc/x 2>/dev/null || echo read-only; exit 7";
    let (mut child, drives) = boot(dir.path(), script, &dir.path().join("api.sock"));
    let out = console(&mut child, Duration::from_secs(30));
    for line in ["hello from the guest", "1000", "/data", "read-only"] {
        assert!(out.lines().any(|l| l.trim() == line), "{line:?} not printed: {out}");
    }
    assert_eq!(ended(&drives[2]), Some(Ended::Exited(7)), "{out}");
    let kept = Command::new("debugfs").args(["-R", "cat /kept"]).arg(&drives[3]).output();
    assert_eq!(String::from_utf8_lossy(&kept.unwrap().stdout).trim(), "kept");
}

/// Sends the guest whose API is `api` Ctrl+Alt+Del, as its driver stops it.
fn stop(api: &Path) {
    let body = r#"{"action_type":"SendCtrlAltDel"}"#;
    ok(Command::new("curl").args(["-sSf", "--unix-socket"]).arg(api).args([
        "-X",
        "PUT",
        "http://localhost/actions",
        "-d",
        body,
    ]));
}

/// A guest running `script`, stopped `times` once it has printed `ready`: what it printed, and
/// how its run ended.
fn stopped(script: &str, times: usize) -> (String, Option<Ended>) {
    let dir = tempfile::tempdir().unwrap();
    let api = dir.path().join("api.sock");
    let (mut child, drives) = boot(dir.path(), script, &api);
    std::thread::sleep(Duration::from_secs(3));
    for _ in 0..times {
        stop(&api);
        std::thread::sleep(Duration::from_millis(500));
    }
    let out = console(&mut child, Duration::from_secs(30));
    assert!(out.contains("ready"), "the entry never ran: {out}");
    (out, ended(&drives[2]))
}

#[test]
fn a_stop_sent_as_ctrl_alt_del_reaches_the_entry_as_sigterm() {
    if machine().is_none() {
        return;
    }
    let script = "trap 'echo stopping; exit 3' TERM; echo ready; while true; do sleep 1; done";
    let (out, ended) = stopped(script, 1);
    assert!(out.contains("stopping"), "{out}");
    assert_eq!(ended, Some(Ended::Exited(3)), "{out}");
}

#[test]
fn a_second_stop_ends_an_entry_that_ignores_the_first() {
    if machine().is_none() {
        return;
    }
    let script = "trap '' TERM; echo ready; while true; do sleep 1; done";
    let (out, ended) = stopped(script, 2);
    assert_eq!(ended, Some(Ended::Signalled(9)), "{out}");
}

/// A connection to the guest's agent through its vsock device's socket at `vsock`, as the driver
/// makes one.
fn connect(vsock: &Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    let mut s = std::os::unix::net::UnixStream::connect(vsock)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.write_all(format!("CONNECT {}\n", control::PORT).as_bytes())?;
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    while byte != *b"\n" {
        s.read_exact(&mut byte)?;
        line.push(byte[0]);
    }
    if !line.starts_with(b"OK ") {
        return Err(std::io::Error::other("not accepted"));
    }
    Ok(s)
}

/// One frame sent to the guest's agent; its answer.
fn ask(vsock: &Path, kind: Kind, payload: &[u8]) -> std::io::Result<(Kind, Vec<u8>)> {
    let mut s = connect(vsock)?;
    control::write(&mut s, kind, payload)?;
    control::read(&mut s)
}

/// Runs `argv` in the guest through its agent for at most `timeout_ms`: what it wrote, and how it
/// ended.
fn exec(vsock: &Path, argv: &[&str], timeout_ms: u64) -> (Vec<u8>, Exited) {
    let argv = argv.iter().map(|a| (*a).to_owned()).collect();
    let asked = serde_json::to_vec(&Exec { argv, timeout_ms, output_limit: 1024 }).unwrap();
    let mut s = connect(vsock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    control::write(&mut s, Kind::Exec, &asked).unwrap();
    let mut output = Vec::new();
    loop {
        match control::read(&mut s).unwrap() {
            (Kind::Output, bytes) => output.extend_from_slice(&bytes),
            (Kind::Exited, end) => return (output, Exited::decode(&end).unwrap()),
            other => panic!("out of turn: {other:?}"),
        }
    }
}

#[test]
fn the_agent_answers_on_its_control_port_and_sends_the_entry_a_signal_named() {
    if machine().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let script = "trap 'echo hup; exit 4' HUP; echo ready; while true; do sleep 1; done";
    let (mut child, drives) = boot(dir.path(), script, &dir.path().join("api.sock"));
    let vsock = dir.path().join("vsock.sock");
    let start = Instant::now();
    let pong = loop {
        if let Ok((Kind::Pong, payload)) = ask(&vsock, Kind::Ping, &[]) {
            break Pong::decode(&payload).unwrap();
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the agent never answered");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(pong.version, control::VERSION);
    std::thread::sleep(Duration::from_secs(1));
    let (said, end) = exec(&vsock, &["/bin/sh", "-c", "id -u; echo oops >&2; exit 3"], 5000);
    assert_eq!(String::from_utf8_lossy(&said), "1000\noops\n", "as the entry's user");
    assert_eq!((end.status, end.timed_out), (3, false));
    let (_, end) = exec(&vsock, &["sleep", "30"], 1000);
    assert_eq!((end.status, end.timed_out), (137, true), "killed for its timeout");
    let (said, end) = exec(&vsock, &["/nonexistent"], 1000);
    assert_eq!(end.status, 127, "not started: {}", String::from_utf8_lossy(&said));
    let (_, resized) = ask(&vsock, Kind::Resize, &control::size(80, 24)).unwrap();
    assert_eq!(Resized::decode(&resized), Some(Resized::NoTerminal), "a console of logs");
    let (_, unknown) = ask(&vsock, Kind::Signal, b"SIGSEGV").unwrap();
    assert_eq!(Signalled::decode(&unknown), Some(Signalled::Unknown), "not one it sends");
    let (kind, payload) = ask(&vsock, Kind::Signal, b"SIGHUP").unwrap();
    assert_eq!((kind, Signalled::decode(&payload)), (Kind::Signalled, Some(Signalled::Sent)));
    let out = console(&mut child, Duration::from_secs(30));
    assert!(out.contains("hup"), "{out}");
    assert_eq!(ended(&drives[2]), Some(Ended::Exited(4)), "{out}");
}
