// SPDX-License-Identifier: AGPL-3.0-or-later

//! The systemd units installation writes (§2 of docs/18-operations.md), one machine's worth: the
//! control plane, and the node beside it as §6 of docs/02-architecture.md divides it, the
//! supervisor as an unprivileged user, the privileged helper as root and socket-activated, and
//! the `oci` driver, which needs containerd's socket and nothing else. Every unit is written
//! from a [`Layout`] alone, so what is written is what a test reads.

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
}

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
    vec![
        ("korpis-server.service", server(l)),
        ("korpis-helper.socket", helper_socket()),
        ("korpis-helper.service", helper(l)),
        ("korpis-driver-oci.service", driver(l)),
        ("korpis-agent.service", agent(l)),
        ("korpis-relay.service", relay(l)),
    ]
}

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
ReadWritePaths=/run/korpis /var/lib/korpis-agent /var/lib/korpis
{HARDENED}
[Install]
WantedBy=multi-user.target
",
        bin = l.bin.display(),
        uid = l.agent_uid,
    )
}

fn agent(l: &Layout) -> String {
    format!(
        "[Unit]
Description=Korpis node agent
After=network-online.target korpis-helper.socket korpis-driver-oci.service
Wants=network-online.target korpis-helper.socket
Requires=korpis-driver-oci.service

[Service]
User=korpis-agent
Group=korpis-agent
ExecStart={bin}/korpis-agent --state-dir {state} run
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
    }
}
