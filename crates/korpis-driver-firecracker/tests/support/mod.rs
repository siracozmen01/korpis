// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the driver's tests share: whether this machine can run a guest, and a driver over a
//! directory of its own with an image of busybox in its content store, a volume per guest, and a
//! console's stream for a guest that has one.

#![allow(dead_code, reason = "each test file uses what it needs")]

use korpis_driver_firecracker::machine::{Machine, START_LOG};
use korpis_driver_firecracker::settings::Settings;
use korpis_driver_kit::exec::{Command as ExecCommand, Ran};
use korpis_image::source::Packing;
use korpis_image::testing::{Entry, image, layer};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Ranges of ids for this test process's guests, above any a host's users take.
pub const IDS: u32 = 1_800_000_000;
pub const MIB: u64 = 1 << 20;
/// The target a guest's programs are built for.
pub const MUSL: &str = "x86_64-unknown-linux-musl";

pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whether this machine can run a guest.
pub fn live() -> bool {
    let tools = repo().join(".tools");
    let kvm = std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok();
    rustix::process::geteuid().is_root()
        && kvm
        && ["bin/firecracker", "bin/jailer", "guest/vmlinux"].iter().all(|p| tools.join(p).exists())
}

pub fn ok(c: &mut Command) {
    let out = c.output().unwrap();
    assert!(out.status.success(), "{c:?}: {}", String::from_utf8_lossy(&out.stderr));
}

pub fn target() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().and_then(Path::parent).and_then(Path::parent).unwrap().to_owned()
}

/// `package`'s example `name`, linked static so a guest's image needs nothing beside it, into the
/// target directory the end-to-end tests build their static programs in, since its flags are not
/// the workspace's. An example builds with its package's dev-dependencies, some of which compile C,
/// so it is built for the host's C library rather than for musl, whose compiler a host may lack.
pub fn example(package: &str, name: &str) -> PathBuf {
    let triple = format!("{}-unknown-linux-gnu", std::env::consts::ARCH);
    let static_target = repo().join("target/static");
    let flags = ["-C", "target-feature=+crt-static", "-C", "debuginfo=0", "-C", "strip=symbols"];
    let mut c = Command::new(env!("CARGO"));
    c.current_dir(repo())
        .args(["build", "-q", "--locked", "-p", package, "--example", name, "--target", &triple])
        .arg("--target-dir")
        .arg(&static_target)
        .env_remove("RUSTFLAGS")
        .env("CARGO_ENCODED_RUSTFLAGS", flags.join("\x1f"));
    ok(&mut c);
    static_target.join(triple).join("debug/examples").join(name)
}

/// `package`'s `bin`, built where this test was, for `triple` if one is named.
pub fn built(package: &str, bin: &str, triple: Option<&str>) -> PathBuf {
    let mut c = Command::new(env!("CARGO"));
    c.args(["build", "-q", "-p", package, "--bin", bin, "--target-dir"]).arg(target());
    if let Some(t) = triple {
        c.args(["--target", t]);
    }
    ok(&mut c);
    triple.map_or_else(|| target().join("debug"), |t| target().join(t).join("debug")).join(bin)
}

/// The state of object `id`, once it is `state`, within `limit`.
pub async fn until(
    m: &Machine,
    id: &str,
    state: driver::RuntimeState,
    limit: Duration,
) -> driver::RuntimeStatus {
    let start = Instant::now();
    loop {
        let s = m.inspect(id).unwrap();
        if s.state.as_known() == Some(state) {
            return s;
        }
        let said = std::fs::read_to_string(m.settings().object(id).join(START_LOG));
        assert!(start.elapsed() < limit, "{id} never reached {state:?}: {s:?}, {said:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// What `argv` run in `id`'s guest wrote, and how it ended.
pub async fn ran(machine: &Machine, id: &str, argv: &[&str]) -> (String, Ran) {
    let argv = argv.iter().map(|a| (*a).to_owned()).collect();
    let command = ExecCommand::checked(argv, Duration::from_secs(10), 1024).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    machine.exec(id, command, tx).await.unwrap();
    let (mut said, mut end) = (Vec::new(), None);
    while let Some(r) = rx.recv().await {
        match r {
            Ran::Output(data) => said.extend(data),
            exit @ Ran::Exit { .. } => end = Some(exit),
        }
    }
    (String::from_utf8(said).unwrap(), end.expect("how it ended"))
}

/// A driver over a directory of its own, and the directories it reads.
pub struct Rig {
    pub dir: tempfile::TempDir,
    pub machine: Machine,
    pub image: String,
}

pub fn settings(dir: &Path) -> Settings {
    static N: AtomicU32 = AtomicU32::new(0);
    let tools = repo().join(".tools");
    Settings {
        firecracker: tools.join("bin/firecracker"),
        jailer: tools.join("bin/jailer"),
        kernel: tools.join("guest/vmlinux"),
        guest: built("korpis-guest", "korpis-guest", Some(MUSL)),
        fsworker: built("korpis-files", "korpis-fsworker", Some(MUSL)),
        install_job: built("korpis-install", "korpis-install-job", Some(MUSL)),
        image_build: built("korpis-image", "korpis-image-build", None),
        writer: built("korpis-stream", "korpis-stream-writer", None),
        content: dir.join("content"),
        volume_root: dir.join("volumes"),
        shared_roots: Vec::new(),
        netns_root: dir.join("netns"),
        identity_root: dir.join("identity"),
        state: dir.join("state"),
        cgroup: format!("korpis-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)),
        cores_host: korpis_driver_kit::cores::Host::Shared,
    }
}

impl Rig {
    pub fn new() -> Self {
        Self::with(&[])
    }

    /// A rig whose image holds `extra` beside busybox, each an executable file at its path.
    pub fn with(extra: &[(&str, &[u8])]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("volumes")).unwrap();
        let busybox = std::fs::read("/bin/busybox").unwrap();
        let names = ["sh", "echo", "cat", "id", "sleep", "grep", "df", "tail", "nc", "ip", "stty"]
            .map(|a| format!("bin/{a}"));
        let mut entries = vec![Entry::File("bin/busybox", &busybox, (0, 0), 0o755)];
        entries.extend(names.iter().map(|n| Entry::Symlink(n, "busybox")));
        entries.extend(extra.iter().map(|(at, bytes)| Entry::File(at, bytes, (0, 0), 0o755)));
        let layers = [(layer(&entries).unwrap(), Packing::Gzip)];
        let image = image(&dir.path().join("content"), &layers, "1000:1000").unwrap();
        let machine = Machine::new(settings(dir.path())).unwrap();
        Self { dir, machine, image }
    }

    /// A volume of its own for the guest with ids from `base`, a block device as the helper makes
    /// one: its image in a directory marked as a block device's.
    pub fn volume(&self, name: &str, base: u32) -> PathBuf {
        use korpis_core::workload::volume::{BLOCK_IMAGE, BLOCK_MARK};
        let dir = self.dir.path().join("volumes").join(name);
        std::fs::create_dir(&dir).unwrap();
        rustix::fs::setxattr(&dir, BLOCK_MARK.0, BLOCK_MARK.1, rustix::fs::XattrFlags::empty())
            .unwrap();
        let path = dir.join(BLOCK_IMAGE);
        ok(Command::new("mkfs.ext4")
            .args(["-q", "-F", "-E", "root_owner=1000:1000"])
            .arg(&path)
            .arg("16M"));
        std::os::unix::fs::chown(&path, Some(base), Some(base)).unwrap();
        path
    }

    /// A spec whose entry is `script`, with a volume at `/data` and 128 MiB.
    pub fn spec(&self, workload: &str, script: &str) -> driver::PreparedSpec {
        static NEXT: AtomicU32 = AtomicU32::new(1);
        let base = IDS + NEXT.fetch_add(1, Ordering::Relaxed) * 65_536;
        let volume = self.volume(workload, base);
        let memory = pb::ReservedLimit { limit: 128 * MIB, ..Default::default() };
        let labels = driver::RuntimeLabels {
            workload: workload.into(),
            intent: "in_1".into(),
            ..Default::default()
        };
        driver::PreparedSpec {
            workload: workload.into(),
            intent: "in_1".into(),
            rootfs: self.image.clone(),
            command: vec!["/bin/sh".into(), "-c".into(), script.into()],
            tier: pb::IsolationTier::ISOLATION_TIER_MICROVM.into(),
            labels: labels.into(),
            uid_base: base,
            uid_count: 65_536,
            resources: pb::ResourceSpec { memory: memory.into(), ..Default::default() }.into(),
            mounts: vec![driver::Mount {
                source: volume.to_string_lossy().into_owned(),
                destination: "/data".into(),
                volume: "data".into(),
                ..Default::default()
            }],
            driver: "firecracker".into(),
            ..Default::default()
        }
    }
}

impl Rig {
    /// `spec` with its console written to its own stream volume, taking input if `input`; and
    /// the stream's directory.
    pub fn console(
        &self,
        mut spec: driver::PreparedSpec,
        input: bool,
    ) -> (driver::PreparedSpec, PathBuf) {
        let volume = korpis_core::workload::stream::StreamKind::Console.volume(&spec.workload);
        let dir = self.dir.path().join("volumes").join(volume);
        std::fs::create_dir_all(&dir).unwrap();
        let stream = driver::StreamCapture {
            dir: dir.to_string_lossy().into_owned(),
            input,
            ..Default::default()
        };
        spec.stream = stream.into();
        (spec, dir)
    }
}

impl Drop for Rig {
    /// Ends whatever a failed test left running, so no guest outlives it: every process in the
    /// rig's cgroups is killed, the cgroups removed, and what its jails bound unbound.
    fn drop(&mut self) {
        korpis_driver_kit::cores::remove_under(&self.machine.settings().cgroup_parent());
        // A guest the test did not destroy keeps its volumes bound in its jail.
        let state = &self.machine.settings().state;
        let mounts = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
        for at in mounts.lines().filter_map(|l| l.split(' ').nth(4)) {
            if Path::new(at).starts_with(state) {
                let _ = rustix::mount::unmount(at, rustix::mount::UnmountFlags::DETACH);
            }
        }
    }
}

/// Whether any service guest of `machine`'s runs: its jails are named `svc-`.
pub fn service_guests(machine: &Machine) -> bool {
    let jails = std::fs::read_dir(machine.settings().jails_of_firecracker());
    jails
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("svc-"))
}

/// Waits until no service guest of `machine`'s runs, within a bound.
pub async fn no_service_guest(machine: &Machine) {
    for _ in 0..300 {
        if !service_guests(machine) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("a service guest still runs");
}
