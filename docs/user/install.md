# Installing Korpis

How to install a control plane and what a node needs. Enrolling nodes and relays is on
[nodes](nodes.md); backing up and restoring the control plane on [recovery](recovery.md). Why the
design is shaped this way is on [availability](../internals/availability.md).

## Contents

- [What you install](#what-you-install)
- [Running the installer](#running-the-installer)
- [Where PostgreSQL comes from](#where-postgresql-comes-from)
- [Serving the panel](#serving-the-panel)
- [What a node needs](#what-a-node-needs)

## What you install

```
korpis-server     the control plane (API, reconcilers, scheduler), gateway, relay, and its own
                  installer; on one machine every role runs in one process
korpis-agent      one signed package: korpis-agent (the supervisor, which dials out and opens no
                  port), korpis-helper (the privileged helper, socket-activated), korpis-fsworker,
                  one korpis-driver-* per runtime driver, and for the microvm tier Firecracker,
                  its jailer, the guest kernel and korpis-guest
PostgreSQL        authoritative state; 13 or later, tested against 17
korpis            the CLI, one static binary
```

On one machine all of it runs on that machine. There is no `curl | sh`: download the signed binary,
verify it against its published digest, and run it. A container quick-start exists for evaluation
and says at startup that it is not a production configuration.

## Running the installer

```
korpis-server install
```

It runs preflight before it changes anything and stops, having changed nothing, when an essential
check fails: PostgreSQL 13 or later (another version than 17 that has every feature the schema uses
is reported unchecked), an empty database, the right to create the schema and roles (not needed when
`--database-url` names a login you made), a data directory that is absolute, not on a tmpfs and not
open to other users, and the control plane's backup target, a directory on another device than the
data directory. A server holding other databases is reported, not refused.

It then makes the users the units run as (`korpis`, `korpis-agent`) through `systemd-sysusers`,
installs into the database, and gives the control plane a login of its own, `korpis_server`, a
member of `korpis_app` owning nothing, so the running service cannot undo the database's
revocations; the owner's credential is the installer's and the migrations' alone. It writes the
units: the control plane; the helper's socket and service; the `oci` driver; the `firecracker`
driver where `/dev/kvm` opens and the microVM files are in `--microvm-dir` (`/usr/lib/korpis`),
saying otherwise why the node runs containers alone; the agent, with `KillMode=process` so workloads
outlive its restart; a relay unit, not enabled on this machine; and the daily control-plane backup.
Last it prints the root credential (a bootstrap invitation, or an API token with `--root-token` for
an unattended install), the backup key once, the first node's join command with an enrollment token
valid for a day, and the `systemctl` commands to run, which it does not run itself.

`--dry-run` prints the report and every file it would write. `--no-system` installs into the
database and data directory alone, for a container or test, and prints the login once.

Until a release package carries the microVM files, `scripts/microvm-files.sh` puts Firecracker, the
jailer, the guest kernel, `korpis-guest`, `korpis-fsworker` and `korpis-install-job` into
`/usr/lib/korpis` from a checkout.

## Where PostgreSQL comes from

The installer asks rather than assumes; unattended, it refuses to guess.

- `--postgres existing` (implied by `--database-owner-url`): a server you run.
- `--postgres package`: the distribution's own, installed with `apt-get` or `dnf` and started with
  systemd. On the Red Hat family it creates the cluster with password authentication over TCP, since
  `ident` admits no URL. Debian 13 packages 17; Ubuntu 24.04 and Fedora 42 package 16, reported
  unchecked.
- `--postgres container`: the official `postgres:17` image pinned by digest, under podman or docker,
  published on loopback alone. It is an evaluation setup: nothing backs it up, upgrades it or
  watches it.

A provisioned server gets an owner, `korpis_owner`, with `CREATEROLE` and not a superuser, owning
the database `korpis`. Its URL goes to `/etc/korpis/owner.env`, root's alone, read by no unit; the
service login's to `/etc/korpis/server.env`.

**Give Korpis a PostgreSQL server of its own.** Events are delivered only once every transaction on
the whole server that could commit an earlier one has ended, so a session idle in a transaction
anywhere on a shared server, or a long index build or `DROP DATABASE`, holds back every event
subscriber. On a shared server, set `idle_in_transaction_session_timeout` for the other
applications' roles. Database availability is PostgreSQL's own (Patroni, a managed service); a
connection pooler in transaction mode makes queues poll, which the control plane logs at start.

## Serving the panel

The panel is a directory of static files built from `web/`, named by `--web-root`
(`KORPIS_WEB_ROOT`) of `korpis-server serve`, served on the API's own address and origin; without it
the address serves the API alone and says so. `--web-origin`, repeatable, names other origins
allowed to call the API with credentials. A relay running as its own process takes the same
`--web-origin`. `/about/source` is always served, since the source offer is owed to every network
user.

Other `serve` settings an operator commonly sets: `--event-retention-days` (30),
`--stream-retention-floor-days` (none; disclosed to every tenant), the rate limits ([rate
limits](../internals/rate-limits.md)), `--metering-interval` and `--metering-lateness`,
`--fetch-allow` for a private registry or mirror network, `--acme-directory`, `--acme-agree-terms`
(no `acme` certificate is issued without it), `--acme-contact`, and `--acme-ca` for a directory with
its own authority.

## What a node needs

`korpis-agent preflight` checks the host against what the node is configured to offer and prints,
for each row, met, not needed, lacking (with what the node offers less of and the command that fixes
it), or unchecked. It writes nothing; `--json` prints the same for a program. It fails only when the
node could offer nothing. **A node that lacks something offers less**: it does not fail to start,
and never offers what it cannot enforce.

| Needs | For | Without it |
|---|---|---|
| Linux 6.1 or later, cgroup v2, user namespaces, seccomp | everything | the node offers nothing |
| containerd | the `oci` driver | the node offers no `container` tier |
| Landlock (5.13+; the port rule 6.7+) | tenant file access | no tenant file API |
| KVM, and the tun driver | `microvm`, `vm` | only `process` and `container`; nested virtualization only matters when the node is itself a VM |
| `io.cost` on the volumes' disk | I/O shares | ceilings only; preflight prints the configuration |
| ext4 with project quota, XFS, or btrfs with qgroups | enforced volume sizes | a `directory` class enforcing nothing, and saying so |
| btrfs (ZFS once built), and `/usr/bin/btrfs` | replicated volumes, holding replicas | replicas are chosen elsewhere |
| `/usr/bin/rsync` | migrating volumes of a class without increments | such a migration is refused, naming the node |
| `mke2fs` | a microVM's volumes, block classes | those volumes are refused |
| a network filesystem mounted where `--shared-class` names | shared storage classes | none offered; a local or root-squashing filesystem is refused |
| `iscsiadm` and `iscsi_tcp`, or the `rbd` module | block storage classes | that `--block-class` is refused at the helper's start |
| nftables 1.0+ with JSON input, IP forwarding | workloads' networks | loopback alone, no endpoints |
| IPv6 forwarding | IPv6 for workloads | IPv4 alone |
| `tc` and HTB, and `--uplink eth0=1gbit` | bandwidth limits; egress reservations need the uplink | nothing that sets them is placed |
| WireGuard (Linux 5.6+), and its UDP port (51820) open between nodes | dependencies across nodes | the node is outside the mesh |
| CDI files under `/etc/cdi` or `/var/run/cdi`, root-owned | devices | no devices offered |
| cpuset partitions (about 6.7+) and a core besides CPU 0's | dedicated cores | none offered, unless drivers run with `--dedicated-cores-host shared` |

Another firewall whose forward chain drops by default (Docker's, ufw's) must accept Korpis's `kw*`
links, which the agent logs at start with the rule to add; Korpis never edits another firewall.
Interrupts not steered away with `irqaffinity=` still run on dedicated cores, and preflight prints
the setting.
