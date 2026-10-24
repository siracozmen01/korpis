// SPDX-License-Identifier: AGPL-3.0-or-later

//! The systemd units installation writes (§2 of docs/18-operations.md), one machine's worth: the
//! control plane, and the node beside it as §6 of docs/02-architecture.md divides it, the
//! supervisor as an unprivileged user, the privileged helper as root and socket-activated, and
//! the `oci` driver, which needs containerd's socket and nothing else, and, where the machine
//! can run guests, the `firecracker` driver. Every unit is written from a [`Layout`] alone, so
//! what is written is what a test reads.

use std::path::PathBuf;

/// Where everything is on this machine, and who runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The directory the binaries are in.
    pub bin: PathBuf,
    /// The control plane's data directory.
    pub data: PathBuf,
    /// The agent's state directory.
    pub agent_state: PathBuf,
    /// The environment file the control plane reads its database URL and settings from.
    pub env_file: PathBuf,
    /// The uid of the agent's user, which the helper and the driver admit.
    pub agent_uid: u32,
    /// The control plane's backup target, which the daily backup writes to.
    pub backup_target: PathBuf,
    /// The directory holding what the microvm tier runs, Firecracker, its jailer, the guest
    /// kernel, `korpis-guest`, `korpis-fsworker`, and `korpis-install-job`, where installation
    /// found the machine able to run guests; `None` where the node offers no microVMs.
    pub microvm: Option<PathBuf>,
}

/// The node's state that is root's: the helper's, and the volumes it makes. Made by
/// installation, as the helper would make it, before any unit names it: systemd refuses to start
/// a unit whose read-write path is missing.
pub const NODE_STATE: &str = "/var/lib/korpis";

/// The users the units run as, for `systemd-sysusers`.
pub const SYSUSERS: &str = "\
# Korpis (§2 of docs/18-operations.md): the control plane, and the node agent's supervisor.
u korpis - \"Korpis control plane\" /var/lib/korpis-server
u korpis-agent - \"Korpis node agent\" /var/lib/korpis-agent
";

/// A unit file: its name and its text.
pub type Unit = (&'static str, String);

/// Every unit, in the order they are written.
#[must_use]
pub fn units(l: &Layout) -> Vec<Unit> {
    let mut all = vec![
        ("korpis-server.service", server(l)),
        ("korpis-helper.socket", helper_socket()),
        ("korpis-helper.service", helper(l)),
        ("korpis-driver-oci.service", driver(l)),
    ];
    if let Some(lib) = &l.microvm {
        all.push(("korpis-driver-firecracker.service", firecracker(l, lib)));
    }
    all.extend([
        ("korpis-agent.service", agent(l)),
        ("korpis-relay.service", relay(l)),
        ("korpis-backup.service", backup(l)),
        ("korpis-backup.timer", BACKUP_TIMER.to_owned()),
    ]);
    all
}

/// The sockets the drivers serve on, each its driver's default.
const OCI_SOCKET: &str = "/run/korpis/driver-oci.sock";
const FIRECRACKER_SOCKET: &str = "/run/korpis/driver-firecracker.sock";

/// What every long-running unit is held to.
const HARDENED: &str = "\
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
LockPersonality=yes
";

fn server(l: &Layout) -> String {
    format!(
        "[Unit]
Description=Korpis control plane
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
User=korpis
Group=korpis
EnvironmentFile={env}
ExecStart={bin}/korpis-server serve
Restart=on-failure
RestartSec=2
ReadWritePaths={data}
CapabilityBoundingSet=
{HARDENED}
[Install]
WantedBy=multi-user.target
",
        env = l.env_file.display(),
        bin = l.bin.display(),
        data = l.data.display(),
    )
}

fn helper_socket() -> String {
    "[Unit]
Description=Korpis privileged helper socket

[Socket]
ListenStream=/run/korpis/helper.sock
SocketUser=root
SocketGroup=korpis-agent
SocketMode=0660
DirectoryMode=0755
Accept=no

[Install]
WantedBy=sockets.target
"
    .to_owned()
}

fn helper(l: &Layout) -> String {
    // Root, and started only when the agent asks: the one privileged process on the node, which
    // takes the socket systemd made as its standard input and admits the agent's user alone.
    format!(
        "[Unit]
Description=Korpis privileged helper
Requires=korpis-helper.socket

[Service]
ExecStart={bin}/korpis-helper --inherit --allow-uid {uid}
StandardInput=socket
StandardOutput=journal
StandardError=journal
",
        bin = l.bin.display(),
        uid = l.agent_uid,
    )
}

fn driver(l: &Layout) -> String {
    // containerd's socket admits root alone unless containerd is given a group, and anyone who
    // can open it can run anything; so the driver is root, with every capability dropped, which
    // is all the socket needs (§6 of docs/02-architecture.md says what this does not stop). Its
    // group is the agent's, so the socket it makes, owner and group only, opens to the agent.
    format!(
        "[Unit]
Description=Korpis oci driver
After=containerd.service
Requires=containerd.service

[Service]
Group=korpis-agent
ExecStart={bin}/korpis-driver-oci --allow-uid {uid}
Restart=on-failure
RestartSec=2
RuntimeDirectory=korpis
RuntimeDirectoryPreserve=yes
StateDirectory=korpis-driver-oci
CapabilityBoundingSet=
ReadWritePaths=/run/korpis {state} {NODE_STATE}
{HARDENED}
[Install]
WantedBy=multi-user.target
",
        bin = l.bin.display(),
        uid = l.agent_uid,
        state = l.agent_state.display(),
    )
}

fn firecracker(l: &Layout, lib: &std::path::Path) -> String {
    // Root, since its jailer makes each guest's chroot, cgroup, and device nodes and drops to the
    // workload's own id before it runs Firecracker; so the cgroup hierarchy is not hidden from it
    // as from the other units, and its capabilities are not bounded. Nor is it held by
    // RestrictSUIDSGID: systemd refuses openat2 and fchmodat2 under it, since it cannot read
    // their mode, and the image build resolves every path of a layer with openat2 and keeps an
    // image's own setuid files on the guest's disk. Its guests outlive it, in cgroups of their
    // own, as containers outlive the oci driver: stopping it ends no guest. Its group is the
    // agent's, for its socket.
    format!(
        "[Unit]
Description=Korpis firecracker driver
After=korpis-helper.socket

[Service]
Group=korpis-agent
ExecStart={bin}/korpis-driver-firecracker --allow-uid {uid} --socket {FIRECRACKER_SOCKET} \\
  --firecracker {lib}/firecracker --jailer {lib}/jailer --kernel {lib}/vmlinux \\
  --guest {lib}/korpis-guest --fsworker {lib}/korpis-fsworker \\
  --install-job {lib}/korpis-install-job --content {state}/content \\
  --identity-root {state}/identity
Restart=on-failure
RestartSec=2
KillMode=process
RuntimeDirectory=korpis
RuntimeDirectoryPreserve=yes
StateDirectory=korpis-driver-firecracker
ReadWritePaths=/run/korpis /sys/fs/cgroup {NODE_STATE}
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
LockPersonality=yes

[Install]
WantedBy=multi-user.target
",
        bin = l.bin.display(),
        uid = l.agent_uid,
        lib = lib.display(),
        state = l.agent_state.display(),
    )
}

fn agent(l: &Layout) -> String {
    let (after, sockets) = match l.microvm {
        Some(_) => (
            " korpis-driver-firecracker.service",
            format!(" --driver-socket {OCI_SOCKET} --driver-socket {FIRECRACKER_SOCKET}"),
        ),
        None => ("", String::new()),
    };
    format!(
        "[Unit]
Description=Korpis node agent
After=network-online.target korpis-helper.socket korpis-driver-oci.service{after}
Wants=network-online.target korpis-helper.socket{after}
Requires=korpis-driver-oci.service

[Service]
User=korpis-agent
Group=korpis-agent
ExecStart={bin}/korpis-agent --state-dir {state} run{sockets}
Restart=on-failure
RestartSec=2
KillMode=process
ReadWritePaths={state}
CapabilityBoundingSet=
{HARDENED}
[Install]
WantedBy=multi-user.target
",
        bin = l.bin.display(),
        state = l.agent_state.display(),
    )
}

fn relay(l: &Layout) -> String {
    // For a machine that is a relay and nothing else (§3.1 of docs/02-architecture.md), joined
    // first with `korpis-server relay join`; on this machine the control plane is the relay, and
    // the unit is written so the same binary carries it, never enabled here.
    format!(
        "[Unit]
Description=Korpis relay, its own process
After=network-online.target
Wants=network-online.target

[Service]
User=korpis
Group=korpis
ExecStart={bin}/korpis-server relay run --dir /var/lib/korpis-relay
Restart=on-failure
RestartSec=2
StateDirectory=korpis-relay
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
AmbientCapabilities=CAP_NET_BIND_SERVICE
{HARDENED}
[Install]
WantedBy=multi-user.target
",
        bin = l.bin.display(),
    )
}

fn backup(l: &Layout) -> String {
    // The control plane's own user, which owns the data directory and is given the target by
    // installation: never root, since the data directory a compromised control plane could change
    // would then steer what root reads and where it writes. It fails when the database is not
    // archived, after writing the keys, so the failure is in the journal and the unit's state.
    format!(
        "[Unit]
Description=Korpis control-plane backup (§6 of docs/18-operations.md)
After=korpis-server.service

[Service]
Type=oneshot
User=korpis
Group=korpis
EnvironmentFile={env}
ExecStart={bin}/korpis-server backup
ReadWritePaths={target}
CapabilityBoundingSet=
{HARDENED}",
        env = l.env_file.display(),
        bin = l.bin.display(),
        target = l.backup_target.display(),
    )
}

/// The backup's schedule: daily, at a time spread over an hour, and caught up after a machine
/// that was off at the time starts again.
const BACKUP_TIMER: &str = "[Unit]
Description=Korpis control-plane backup, daily

[Timer]
OnCalendar=daily
RandomizedDelaySec=1h
Persistent=true

[Install]
WantedBy=timers.target
";

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout {
            bin: "/usr/local/bin".into(),
            data: "/var/lib/korpis-server".into(),
            agent_state: "/var/lib/korpis-agent".into(),
            env_file: "/etc/korpis/server.env".into(),
            agent_uid: 991,
            backup_target: "/mnt/backup".into(),
            microvm: None,
        }
    }

    #[test]
    fn the_helper_and_the_driver_admit_the_agent_s_user_and_only_the_helper_keeps_root_s_powers() {
        let all = units(&layout());
        let text = |name: &str| all.iter().find(|(n, _)| *n == name).unwrap().1.clone();
        assert!(text("korpis-helper.service").contains("--inherit --allow-uid 991"));
        assert!(text("korpis-driver-oci.service").contains("--allow-uid 991"));
        assert!(text("korpis-driver-oci.service").contains("CapabilityBoundingSet=\n"));
        assert!(text("korpis-driver-oci.service").contains("Group=korpis-agent"), "its socket");
        assert!(!text("korpis-helper.service").contains("CapabilityBoundingSet"));
        assert!(text("korpis-agent.service").contains("User=korpis-agent"));
        assert!(text("korpis-agent.service").contains("KillMode=process"), "workloads outlive it");
        assert!(text("korpis-server.service").contains("EnvironmentFile=/etc/korpis/server.env"));
        assert!(text("korpis-helper.socket").contains("SocketGroup=korpis-agent"));
        assert!(text("korpis-relay.service").contains("relay run --dir /var/lib/korpis-relay"));
        assert!(text("korpis-backup.service").contains("ReadWritePaths=/mnt/backup"));
        assert!(text("korpis-backup.service").contains("User=korpis\n"), "never root");
        assert!(text("korpis-backup.timer").contains("Persistent=true"));
    }

    #[test]
    fn a_node_that_runs_guests_has_the_firecracker_driver_and_its_agent_asks_both_drivers() {
        let plain = units(&layout());
        assert!(!plain.iter().any(|(n, _)| *n == "korpis-driver-firecracker.service"));
        let agent = |all: &[Unit]| {
            all.iter().find(|(n, _)| *n == "korpis-agent.service").unwrap().1.clone()
        };
        assert!(agent(&plain).contains("run\n"), "the oci driver's socket by default");
        let all = units(&Layout { microvm: Some("/usr/lib/korpis".into()), ..layout() });
        let (_, fc) = all.iter().find(|(n, _)| *n == "korpis-driver-firecracker.service").unwrap();
        assert!(fc.contains("--kernel /usr/lib/korpis/vmlinux") && fc.contains("--allow-uid 991"));
        assert!(fc.contains("KillMode=process"), "its guests outlive it");
        let identity = "--identity-root /var/lib/korpis-agent/identity";
        assert!(fc.contains(identity), "the agent's identity root, whose sockets it relays");
        assert!(!fc.contains("RestrictSUIDSGID"), "the image build's openat2 would be refused");
        assert!(fc.contains("ReadWritePaths=/run/korpis /sys/fs/cgroup /var/lib/korpis"));
        let a = agent(&all);
        let sockets =
            format!("run --driver-socket {OCI_SOCKET} --driver-socket {FIRECRACKER_SOCKET}");
        assert!(a.contains(&sockets), "both drivers asked: {a}");
        assert!(a.contains("korpis-helper.socket korpis-driver-firecracker.service\n"), "{a}");
    }
}
