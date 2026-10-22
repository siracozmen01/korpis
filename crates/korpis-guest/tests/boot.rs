// SPDX-License-Identifier: AGPL-3.0-or-later

//! A microVM booted as its driver will boot one (§6 of docs/04-runtimes.md): the node's guest
//! kernel (scripts/guest-kernel.sh), a boot drive holding nothing but `korpis-guest`, an image
//! drive, the config drive, and a volume. The entry runs as its user with its environment, writes
//! its volume, and its exit status comes back on the config drive; a stop sent as Ctrl+Alt+Del
//! reaches it as `SIGTERM`. Needs root, /dev/kvm, Firecracker (scripts/tools.sh), the guest
//! kernel, and mkfs.ext4 and debugfs, and is skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_guest::config::{Config, Volume};
use korpis_guest::status::Ended;
use std::collections::BTreeMap;
use std::io::Read as _;
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

/// `korpis-guest`, built static where this test was built.
fn guest() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let target = exe.parent().and_then(Path::parent).and_then(Path::parent).unwrap();
    let triple = "x86_64-unknown-linux-musl";
    ok(Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", "korpis-guest", "--bin", "korpis-guest", "--target", triple])
        .arg("--target-dir")
        .arg(target));
    target.join(triple).join("debug/korpis-guest")
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
    let boot = dir.join("boot");
    for d in ["dev", "lower", "upper", "newroot"] {
        std::fs::create_dir_all(boot.join(d)).unwrap();
    }
    std::fs::copy(guest(), boot.join("korpis-guest")).unwrap();
    let image = dir.join("image");
    std::fs::create_dir_all(image.join("bin")).unwrap();
    std::fs::copy("/bin/busybox", image.join("bin/busybox")).unwrap();
    for applet in ["sh", "echo", "id", "sleep", "cat"] {
        std::os::unix::fs::symlink("busybox", image.join("bin").join(applet)).unwrap();
    }
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let paths = ["boot.ext4", "image.ext4", "config", "data.ext4"].map(|n| dir.join(n));
    ext4(&boot, &paths[0], 16, "0:0");
    ext4(&image, &paths[1], 16, "0:0");
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
    };
    config.write(&mut std::fs::File::create(&paths[2]).unwrap()).unwrap();
    paths
}

/// Firecracker's configuration for `drives` and `kernel`.
fn vm(kernel: &Path, drives: &[PathBuf; 4]) -> String {
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
    })
    .to_string()
}

/// A guest booted with `script` as its entry, its API on `api`: the process, its drives.
fn boot(dir: &Path, script: &str, api: &Path) -> (std::process::Child, [PathBuf; 4]) {
    let (firecracker, kernel) = machine().unwrap();
    let drives = drives(dir, script);
    let config = dir.join("vm.json");
    std::fs::write(&config, vm(&kernel, &drives)).unwrap();
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
