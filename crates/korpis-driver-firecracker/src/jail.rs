// SPDX-License-Identifier: AGPL-3.0-or-later

//! A guest's jail: the chroot Firecracker's jailer runs it in, holding the kernel, the drives in
//! the order the guest counts them (`korpis_guest::drive`), and the machine's configuration, as
//! hard links to what the driver keeps, and the volumes, which are on the node's storage where a
//! hard link from the driver's state cannot reach, bound in and unbound with the jail; and the
//! jailer's command line, which drops to the workload's own id, makes its cgroup with the guest's
//! limits, joins the workload's network namespace, whose tap is the guest's `eth0` (§4.1 of
//! docs/07-networking.md), and detaches it so the guest outlives the driver. Paths in the configuration are the chroot's, as the jailed Firecracker
//! sees them.

use crate::settings::Settings;
use korpis_core::workload::network::TAP;
use std::io;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The kernel's command line: the serial port as the console, a reset as the end (Firecracker
/// exits on it, §6 of docs/04-runtimes.md), the boot drive as a read-only root, and
/// `korpis-guest` as the first process.
pub const BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 pci=off quiet loglevel=3 \
                             root=/dev/vda ro init=/korpis-guest";

/// The API socket, as the jailed Firecracker sees it.
pub const API: &str = "/api.socket";

/// The vsock device's socket, as the jailed Firecracker sees it: a connection on it reaches a
/// port of the guest's agent (§6 of docs/04-runtimes.md).
pub const VSOCK: &str = "/vsock.socket";

/// The vsock device's socket of object `id`'s Firecracker, as the driver reaches it.
#[must_use]
pub fn vsock_socket(settings: &Settings, id: &str) -> PathBuf {
    settings.chroot(id).join(VSOCK.trim_start_matches('/'))
}

/// The API socket of object `id`'s Firecracker, as the driver reaches it.
#[must_use]
pub fn api_socket(settings: &Settings, id: &str) -> PathBuf {
    settings.chroot(id).join(API.trim_start_matches('/'))
}

/// What a guest is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    /// Its vCPUs.
    pub vcpus: u32,
    /// Its memory, in MiB.
    pub memory_mib: u64,
    /// The cgroup v2 settings its Firecracker runs under, as `file=value`.
    pub cgroup: Vec<String>,
    /// The network namespace its Firecracker joins, holding the tap that is its `eth0`; none
    /// for a guest with loopback alone.
    pub netns: Option<PathBuf>,
    /// Whether it holds dedicated cores, so its cgroup is under the driver's `dedicated` one.
    pub dedicated: bool,
}

/// What the jail holds, by the host's paths.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Drives {
    /// The kernel.
    pub kernel: PathBuf,
    /// The boot drive.
    pub boot: PathBuf,
    /// The image's drive.
    pub image: PathBuf,
    /// The object's config drive.
    pub config: PathBuf,
    /// Each volume's drive, and whether it is read-only, in the config's order.
    pub volumes: Vec<(PathBuf, bool)>,
}

/// The machine's configuration, by the chroot's paths.
fn machine(shape: &Shape, volumes: &[(PathBuf, bool)]) -> serde_json::Value {
    let drive = |id: String, path: String, root: bool, ro: bool| {
        serde_json::json!({"drive_id": id, "path_on_host": path, "is_root_device": root,
                           "is_read_only": ro})
    };
    let mut drives = vec![
        drive("boot".into(), "/boot.ext4".into(), true, true),
        drive("image".into(), "/image.ext4".into(), false, true),
        drive("config".into(), "/config".into(), false, false),
    ];
    for (i, (_, ro)) in volumes.iter().enumerate() {
        drives.push(drive(format!("volume-{i}"), format!("/volume-{i}"), false, *ro));
    }
    let mut machine = serde_json::json!({
        "boot-source": {"kernel_image_path": "/vmlinux", "boot_args": BOOT_ARGS},
        "drives": drives,
        "machine-config": {"vcpu_count": shape.vcpus, "mem_size_mib": shape.memory_mib},
        "vsock": {"guest_cid": korpis_guest::control::GUEST_CID, "uds_path": VSOCK},
    });
    if let (Some(_), Some(m)) = (&shape.netns, machine.as_object_mut()) {
        let tap = serde_json::json!([{"iface_id": "eth0", "host_dev_name": TAP}]);
        m.insert("network-interfaces".into(), tap);
    }
    machine
}

/// Links `from` into the chroot `root` as `name`, replacing what a previous attempt left.
fn link(from: &Path, root: &Path, name: &str) -> io::Result<()> {
    let to = root.join(name);
    match std::fs::remove_file(&to) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::hard_link(from, &to)
        .map_err(|e| io::Error::new(e.kind(), format!("{} into the jail: {e}", from.display())))
}

/// Binds the volume `from` into the chroot `root` as `name`, from a descriptor of the file
/// itself, so what is bound is what was checked.
fn bind(from: &Path, root: &Path, name: &str) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    let to = root.join(name);
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&to)?;
    let held = crate::volume::held(from)?;
    rustix::mount::mount_bind(format!("/proc/self/fd/{}", held.as_raw_fd()), &to)
        .map_err(|e| io::Error::other(format!("{} into the jail: {e}", from.display())))
}

/// Unbinds every volume in the chroot `root`. The chroot is the guest's to write, so each is
/// unbound without following a link: a link it put there names nothing the driver unbinds.
fn unbind(root: &Path) -> io::Result<()> {
    use rustix::mount::{UnmountFlags, unmount};
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("volume-") {
            continue;
        }
        match unmount(entry.path(), UnmountFlags::DETACH | UnmountFlags::NOFOLLOW) {
            Ok(()) | Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOENT) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Builds object `id`'s chroot from `drives`, configured as `shape` says. It is built again for
/// every run, with nothing kept from the last: the guest's Firecracker can write its chroot, so
/// what one run left there is the guest's, and neither the jailer, which works in the chroot as
/// root, nor the driver reads it.
pub fn build(settings: &Settings, id: &str, drives: &Drives, shape: &Shape) -> io::Result<()> {
    let root = settings.chroot(id);
    std::fs::create_dir_all(&root)?;
    link(&drives.kernel, &root, "vmlinux")?;
    link(&drives.boot, &root, "boot.ext4")?;
    link(&drives.image, &root, "image.ext4")?;
    link(&drives.config, &root, "config")?;
    for (i, (volume, _)) in drives.volumes.iter().enumerate() {
        bind(volume, &root, &format!("volume-{i}"))?;
    }
    let config = serde_json::to_vec(&machine(shape, &drives.volumes)).map_err(io::Error::other)?;
    std::fs::write(root.join("vm.json"), config)
}

/// The jailer's command for object `id`, its Firecracker running as `(uid, gid)`.
#[must_use]
pub fn command(settings: &Settings, id: &str, (uid, gid): (u32, u32), shape: &Shape) -> Command {
    let mut c = Command::new(&settings.jailer);
    c.args(["--id", id, "--exec-file"])
        .arg(&settings.firecracker)
        .args(["--uid", &uid.to_string(), "--gid", &gid.to_string(), "--chroot-base-dir"])
        .arg(settings.jails())
        .args([
            "--cgroup-version",
            "2",
            "--parent-cgroup",
            &settings.parent_cgroup(shape.dedicated),
        ]);
    for setting in &shape.cgroup {
        c.args(["--cgroup", setting]);
    }
    if let Some(netns) = &shape.netns {
        c.arg("--netns").arg(netns);
    }
    // In a pid namespace of its own, so its Firecracker can name no process but itself. The
    // jailer forks to do it and exits, and the process that is Firecracker is named in the pid
    // file it leaves in the chroot. It is not daemonized, which would put nothing on the guest's
    // serial port: the caller gives it the console as its standard input and output, and a
    // process group of its own, so the guest outlives the driver.
    c.args(["--new-pid-ns", "--", "--config-file", "/vm.json", "--api-sock", API])
        .args(["--level", "Error"]);
    c.process_group(0);
    c
}

/// Where the jailer leaves the process id of object `id`'s Firecracker.
#[must_use]
pub fn pid_file(settings: &Settings, id: &str) -> PathBuf {
    let name = settings.firecracker.file_name().unwrap_or_default().to_string_lossy();
    settings.chroot(id).join(format!("{name}.pid"))
}

/// Removes object `id`'s jail and its cgroup; what is already gone is removed.
pub fn remove(settings: &Settings, id: &str) -> io::Result<()> {
    unbind(&settings.chroot(id))?;
    match std::fs::remove_dir_all(settings.jail(id)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    remove_cgroup(&settings.cgroup_of(id))
}

/// Removes the cgroup at `path`, once the console's writer has sealed what its guest wrote and
/// left it; one that has not within a bound is ended, and one still there a bound later said.
fn remove_cgroup(path: &Path) -> io::Result<()> {
    use std::time::{Duration, Instant};
    let start = Instant::now();
    let mut killed = false;
    loop {
        match std::fs::remove_dir(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) if e.raw_os_error() != Some(rustix::io::Errno::BUSY.raw_os_error()) => {
                return Err(e);
            }
            Err(e) if killed && start.elapsed() > Duration::from_secs(10) => return Err(e),
            Err(_) if !killed && start.elapsed() > Duration::from_secs(5) => {
                std::fs::write(path.join("cgroup.kill"), "1")?;
                killed = true;
            }
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_names_its_drives_in_the_order_the_guest_counts_them() {
        let shape =
            Shape { vcpus: 2, memory_mib: 256, cgroup: Vec::new(), netns: None, dedicated: false };
        let m = machine(&shape, &[(PathBuf::from("/v/a"), false), (PathBuf::from("/v/b"), true)]);
        let ids: Vec<&str> = m["drives"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["drive_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["boot", "image", "config", "volume-0", "volume-1"]);
        assert_eq!(m["drives"][4]["is_read_only"], true);
        assert_eq!(m["machine-config"]["vcpu_count"], 2);
        assert_eq!(m["vsock"]["uds_path"], VSOCK, "the agent's channel, in the chroot");
        assert!(m["boot-source"]["boot_args"].as_str().unwrap().contains("init=/korpis-guest"));
        assert!(m.get("network-interfaces").is_none(), "loopback alone");
    }

    #[test]
    fn a_guest_given_a_network_holds_its_namespace_s_tap_and_its_firecracker_joins_it() {
        let netns = PathBuf::from("/run/korpis/netns/w1/net");
        let shape = Shape {
            vcpus: 1,
            memory_mib: 128,
            cgroup: Vec::new(),
            netns: Some(netns),
            dedicated: false,
        };
        let m = machine(&shape, &[]);
        assert_eq!(m["network-interfaces"][0]["host_dev_name"], TAP);
        assert_eq!(m["network-interfaces"][0]["iface_id"], "eth0");
        let settings = crate::settings::Settings {
            firecracker: "/fc".into(),
            jailer: "/jailer".into(),
            kernel: "/k".into(),
            guest: "/g".into(),
            fsworker: "/f".into(),
            install_job: "/j".into(),
            image_build: "/b".into(),
            writer: "/w".into(),
            content: "/c".into(),
            volume_root: "/v".into(),
            shared_roots: Vec::new(),
            block_root: "/var/lib/korpis/block".into(),
            netns_root: "/run/korpis/netns".into(),
            identity_root: "/i".into(),
            state: "/s".into(),
            cgroup: "korpis".into(),
            cores_host: korpis_driver_kit::cores::Host::Kept,
        };
        let c = command(&settings, "fc-1", (1 << 20, 1 << 20), &shape);
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let at = args.iter().position(|a| a == "--netns").unwrap();
        assert_eq!(args[at + 1], "/run/korpis/netns/w1/net");
        assert!(
            at < args.iter().position(|a| a == "--").unwrap(),
            "the jailer's, not Firecracker's"
        );
    }
}
