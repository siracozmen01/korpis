// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the driver's tests share: whether this machine can run a guest, and a driver over a
//! directory of its own with an image of busybox in its content store, a volume per guest, and a
//! console's stream for a guest that has one.

#![allow(dead_code, reason = "each test file uses what it needs")]

use korpis_driver_firecracker::machine::Machine;
use korpis_driver_firecracker::settings::Settings;
use korpis_image::source::Packing;
use korpis_image::testing::{Entry, image, layer};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

/// Ranges of ids for this test process's guests, above any a host's users take.
pub const IDS: u32 = 1_800_000_000;
pub const MIB: u64 = 1 << 20;

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
        guest: built("korpis-guest", "korpis-guest", Some("x86_64-unknown-linux-musl")),
        image_build: built("korpis-image", "korpis-image-build", None),
        writer: built("korpis-stream", "korpis-stream-writer", None),
        content: dir.join("content"),
        volume_root: dir.join("volumes"),
        state: dir.join("state"),
        cgroup: format!("korpis-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)),
    }
}

impl Rig {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("volumes")).unwrap();
        let busybox = std::fs::read("/bin/busybox").unwrap();
        let names =
            ["sh", "echo", "cat", "id", "sleep", "grep", "df", "tail"].map(|a| format!("bin/{a}"));
        let mut entries = vec![Entry::File("bin/busybox", &busybox, (0, 0), 0o755)];
        entries.extend(names.iter().map(|n| Entry::Symlink(n, "busybox")));
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
    /// rig's cgroups is killed, and the cgroups removed.
    fn drop(&mut self) {
        let parent = self.machine.settings().cgroup_parent();
        let _ = std::fs::write(parent.join("cgroup.kill"), "1");
        let children: Vec<_> = std::fs::read_dir(&parent)
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
            .unwrap_or_default();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        for child in children {
            while std::fs::remove_dir(&child).is_err() && std::time::Instant::now() < until {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        let _ = std::fs::remove_dir(parent);
    }
}
