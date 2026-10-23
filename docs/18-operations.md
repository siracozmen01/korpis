# Operations

**Status:** design **Date:** 2026-08-07 **Depends on:** [`03-state.md`](./03-state.md),
[`02-architecture.md`](./02-architecture.md), [`05-scheduling.md`](./05-scheduling.md)
**Implements:** Principles P4, P9

---

## 1. The governing constraint

> **A single operator with one machine must be able to install and run Korpis.**

§2 of `00-overview.md` states this as a boundary against Kubernetes' operational cost, and it is
the constraint every decision in this document is checked against. Korpis has a large model; it is
not permitted to have a large *installation*. Every feature here must degrade to one machine, one
operator, and no cluster.

The corollary matters as much: the same binaries, in the same configuration model, scale to a
fleet. There is no "single-node mode" that is a different product with a different upgrade path,
because that path is how a project ends up with a hobbyist edition nobody tests and a production
edition nobody can afford.

---

## 2. Install, honestly

Two signed artifacts and a database:

```
korpis-server     one binary: control plane (API, reconcilers, scheduler), gateway, relay, and
                  its own installer. Roles are enabled per process; on one machine, all of them.
korpis-agent      one signed package of separate executables (§6.2 of `02-architecture.md`):
                    korpis-agent          the supervisor; dials out; no inbound ports
                    korpis-helper         the privileged helper, socket-activated
                    korpis-fsworker       the per-workload filesystem worker
                    korpis-driver-*       one per runtime driver the node offers
                    korpis-guest          placed inside microVMs, never run on the host
PostgreSQL        authoritative state (§6 of `03-state.md`. Postgres only, no SQLite, no MySQL).
korpis            the CLI, one static binary, for whoever operates the installation
```

On one machine, all of it runs on one machine. That is the entire single-node story.

> Second external review, finding S55. Recorded in §17 of `23-walkthroughs.md`. This section said
> "two binaries and a database" while the agent it describes is a supervisor, a root helper,
> filesystem workers, and one process per driver (§2 of `21-stack.md`). An
> operator installs two artifacts; the agent artifact contains several executables because the
> privilege separation of §6 of `02-architecture.md` is made of process boundaries. Saying so costs
> nothing and makes the process list on a node unsurprising.

**The database is the one place Korpis cannot claim "just two artifacts", and it says so rather
than pretending.** §6 of `03-state.md` killed SQLite deliberately: the transactional guarantees that
make `Intent`/`Effect` honest are not optional, and an embedded engine that cannot provide them
would undermine the model in exchange for a nicer README. The installer provisions Postgres through
the system package manager or a container and gets out of the way; Korpis does not vendor, fork, or
supervise a database it did not write.

**Give it a PostgreSQL server of its own, or bound idle transactions on the one it shares.** The
event log hands out an event only once every transaction that could still commit an earlier one
has ended, and PostgreSQL knows that only for the whole server, not per database (§5 of
`15-observability.md`). A session left `idle in transaction` by anything on the server, another
application's included, stops every subscriber's delivery until it ends; nothing is lost, but
extensions and integrations see nothing new. On a shared server, set
`idle_in_transaction_session_timeout` for the other applications' roles. That bounds only the
idle case. A transaction that is working, not idle, holds delivery back just the same for as long
as it runs: an index built or a table loaded in one statement, or a `DROP DATABASE`, which waits
for a checkpoint and on a busy server takes seconds, many of them at once longer still. No
timeout separates those from legitimate work, which is the strongest reason for a server of its
own.

> Corrected 2026-10-13, from the test suite: its teardowns drop a database per test, dozens at
> once, and each held every other test's events back for up to ten seconds, which this paragraph,
> naming only idle sessions, did not predict. The event log keeps
events for `--event-retention-days` (30 by default) of `korpis-server serve`; effects, the audit
record, are kept regardless (§4 of `15-observability.md`). `--stream-retention-floor-days` (none
by default) is the operator's floor on how long offloaded console segments are kept, disclosed to
every tenant with the stream (§8 of `17-security.md`).

The panel is a directory of static files, built from `web/` and named by `--web-root`
(`KORPIS_WEB_ROOT`) of `korpis-server serve`; the control plane serves it on the API's own address
and origin (§1 of `11-surface-web.md`). Without it the address serves the API alone and says so at
start. `--web-origin`, repeatable, names each other origin allowed to call the API with
credentials, which a relying party's own page needs. `/about/source` is served either way, because
the source offer of §2 of `19-governance.md` is owed to every network user, not only to panel
users. A relay that is its own process takes the same `--web-origin` (`korpis-server relay run`),
so the panel can open console and file sessions at it, and answers every portal's origin, which its
feed names (§3.6 of `07-networking.md`); the control plane names its relays in the panel page's
policy as it serves it, and a panel built to be served elsewhere names them at build time with
`VITE_KORPIS_RELAYS` beside `VITE_KORPIS_API`.

### The binary is its own installer

> Resolves open question 1 of §10.

```
korpis-server install
```

Download one signed binary, verify it against the published digest, run it. It runs preflight, asks
where PostgreSQL should come from rather than assuming, generates the signing keys, writes the
systemd units, and prints the join command for the first node and the bootstrap invitation that
makes its first user root (§9.1 of `08-identity.md`; `--root-token` instead issues root to an API
token, for an unattended install). Then it is the same binary that runs
the control plane, there is no separate installer artifact to drift out of sync with what it
installs.

> As built, 2026-10-17 (`crates/korpis-server/src/install`). `korpis-server install` runs
> preflight before it changes anything, and stops, having changed nothing, when an essential check
> lacks: PostgreSQL 13 or later (17 is the version it is tested against; another that has every
> feature the schema uses is reported unchecked, not met), an empty database, the right to create
> the schema, the right to create roles (not needed when `--database-url` names a login the
> operator made), the data directory (absolute, not on a tmpfs, and not open to other users), and
> the control plane's backup target of §6. A server holding other databases is reported and not
> refused, with the timeout the paragraph above names. It then makes the users the units run as,
> `korpis` and `korpis-agent`, from a `sysusers.d` file through `systemd-sysusers`; installs into
> the database; and gives the control plane a login of its own, `korpis_server`, a member of
> `korpis_app` that owns nothing, so the credential a running control plane holds cannot undo the
> revocations of §3.1 of `03-state.md`. The owner's credential is the installer's and the
> migrations', never the running service's. The login's URL goes into `/etc/korpis/server.env`,
> readable by root alone. The units it writes are the control plane; the helper's socket and
> service, root and socket-activated, admitting the agent's uid; the `oci` driver, root with every
> capability dropped (§6 of `02-architecture.md` says what that does not stop); the agent, as
> `korpis-agent`, with `KillMode=process` so workloads outlive its restart; and a relay unit for a
> machine that is a relay alone, never enabled on this one, where the control plane is the relay.
> Last it prints the root credential, the backup key once, the first node's join command with a
> one-time enrollment token valid for a day, and the `systemctl` commands to run, which it does
> not run itself. `--dry-run` prints the report and every file it would write; `--no-system`
> installs into the database and the data directory alone, for a container or a test whose
> processes something else starts, and prints the login it made once, since no file keeps it.
> The backup
> target is a directory, checked to be writable and on another device than the data directory,
> which a bind mount of the same filesystem is not and another partition of the same disk is, so
> that one passes; an S3 target is refused as not built. Declaring a target records it beside the
> data directory and makes the key its backup is sealed with; writing the backup there is the
> control-plane backup of §6, `korpis-server backup`, which said "not built yet" here until
> 2026-10-19 though it was built on 2026-10-18 (the as-built note in §6).

> As built, 2026-10-17 (`crates/korpis-server/src/install/postgres`). Where PostgreSQL comes
> from is decided first: `--postgres existing`, implied by `--database-owner-url`, names a server
> the operator runs; `--postgres package` installs the distribution's own with `apt-get` or `dnf`
> and starts it with systemd; `--postgres container` runs the official `postgres:17` image,
> pinned by its index digest, with podman or docker, published on loopback alone. On a terminal
> with neither flag nor URL the installer asks; unattended, it refuses rather than guess. A
> server it provisions is provisioned only after the checks that need no server, the data
> directory and the backup target, have passed, and is given the same owner either way:
> `korpis_owner`, with `CREATEROLE` and not a superuser, owning a database `korpis`, made as the
> server's superuser over its local socket; everything after is what installation does on any
> server. The owner's URL, which an upgrade migrates with, goes to `/etc/korpis/owner.env`, root's
> alone and read by no unit. What each path verifies, and what it cannot: the package path takes
> whatever major the distribution packages, and preflight then says whether it is the tested one
> (Debian 13 packages 17; Ubuntu 24.04 and Fedora 42 package 16, reported unchecked); on the Red
> Hat family it creates the cluster itself with password authentication over TCP, since that
> family's default, `ident`, admits no URL, and elsewhere it relies on the distribution's default
> of passwords on loopback, which it does not check. `scripts/test-install-package.sh` runs the
> whole installer, units started, in a systemd container of Debian 13, Ubuntu 24.04, and Fedora
> 42; no other distribution is tried. The container path is tested against docker on every
> commit; with podman, which has no daemon to restart it, a unit runs the container, and that is
> not tested here. A container is said, when it is made, to be an evaluation setup: nothing backs
> it up, upgrades it, or watches it, and its image is the one pinned in the release, updated only
> by a commit that changes the digest.

**There is no `curl | sh`.** It is what everyone in this market ships and it is inconsistent with
being a project whose recipes reject a `fetch` without a hash (§4 of `09-recipes.md`), whose
releases are reproducible and signed (§11 of `17-security.md`), and which tells operators to verify
artifacts by digest. A one-liner that pipes an unverified script into a shell teaches the opposite
of every other thing Korpis asks of its users. Download, verify, run is three commands instead of
one, and the three are honest.

A container-compose quick-start also exists for evaluation. It **says on startup that it is not a
production configuration** and why, an unsupervised database in a container is precisely the setup
nobody tests and everybody eventually runs. Labelling it is P4 applied to our own packaging.

Distribution packages (`deb`, `rpm`) with a signed repository follow when there is someone to
maintain them. They are a better answer than the installer for operators who already manage
machines with configuration management, and a worse one to start with, because a stale package is a
support burden that outlives the person who built it.

### Preflight

`korpis-agent preflight` reports what a host is missing, kernel version, cgroup v2, user
namespaces, Landlock availability, the filesystem features a storage class needs, and prints
exactly what to run to fix each. **It does not silently modify the host.** An installer that loads
kernel modules and rewrites `sysctl` on someone's machine is a configuration management system, and
Korpis is not one.

> As built, 2026-10-17 (`crates/korpis-agent/src/preflight`). `korpis-agent preflight` checks
> each row of the table below that a node can see, against the driver it is configured with: the
> kernel floor, cgroup v2 and the cpu, memory, pids, and io controllers, user namespaces, seccomp,
> Landlock (by making rulesets it never applies, so it names the newest ABI whose file access
> rights the kernel handles), KVM and, only inside a virtual machine, nested virtualization,
> io.cost on the disk the volumes are on (a partition followed to its disk, which is where
> io.cost is configured), containerd for the `oci` driver, the storage classes the helper
> declares, nftables 1.0 or later with its JSON input, IP forwarding, and IPv6. Each is met, not
> needed for this node's offer, lacking (with what the node offers less of and the command that
> fixes it), or unchecked (with what would let it be checked). It reads, and asks `nft` to list
> the ruleset; it writes nothing, and `--json` prints the same for a program. It fails only when
> the node could offer nothing: the kernel floor, cgroup v2, user namespaces, seccomp, or, for the
> `oci` driver, containerd; anything else lacking is a node that offers less. Storage is the
> helper's to say, since the helper makes the volumes, so preflight asks it rather than judging
> the filesystem a second time, and says so when it cannot reach it. What it does not check: the
> `stable` exposure mode's edge failover group or routed pool, which is the control plane's
> configuration rather than the node's, and KVM is reported but needed by no driver built yet.

### What a node actually requires

Requirements are a function of the isolation tiers and storage classes a node offers, not a single
number, a node that only runs `process`-tier workloads on ext4 needs far less than one offering
microVMs on ZFS. Preflight reports against what the node is *configured to offer*.

| | Required for | Notes |
|---|---|---|
| Linux kernel **6.1+** | everything | 5.13 has Landlock at all; 6.1 LTS is the realistic floor, and later ABI versions confine more |
| cgroup v2, unified hierarchy | every tier | the enforcement path for CPU, memory, PID, and I/O (§10 of `17-security.md`) |
| user namespaces enabled | every tier | root inside a workload must not be root on the host |
| `openat2` (5.6+) | tenant file access | subsumed by the kernel floor |
| Landlock | tenant file access | the filesystem worker's confinement; without it the node offers no tenant file API |
| **KVM** | `microvm`, `vm` | absent → the node advertises only `process` and `container`. On bare metal KVM is enough; **nested virtualization is needed only when the node is itself a virtual machine**, and preflight checks it only then (finding S67) |
| `io.cost` configured for the volume devices | I/O shares (§3 of `06-storage.md`) | absent → I/O ceilings only; preflight prints the configuration to apply and does not apply it |
| an edge failover group, or a routed address pool | the `stable` exposure mode (§3.1 of `07-networking.md`) | absent → `node_bound` is offered instead, and says what it does not survive |
| ZFS or btrfs | replicated and snapshot-capable storage classes | ext4/XFS with project quota still gives enforced byte and inode limits |
| IPv6 | the default addressing mode (§4 of `07-networking.md`) | absent → IPv4 only, and the scarcity is the operator's problem |
| nftables 1.0+ with its JSON input, and IP forwarding | every workload's network (§4.1 of `07-networking.md`) | absent → the node's workloads have loopback alone, and no endpoint is offered on it; another firewall whose forward chain drops by default (Docker's, ufw's) must accept the `kw*` links, which the agent logs and does not do |
| `tc` from iproute2 and the `htb` qdisc | ingress limits and an endpoint's own limits, held on the workload's link (§7 of `07-networking.md`) | absent → the node enforces no bandwidth and the scheduler places nothing that sets one there, saying why; an endpoint's accounting needs only nftables, above |
| those, and an uplink declared with `korpis-agent run --uplink eth0=1gbit` | egress reservations and limits (§7 of `07-networking.md`) | absent → the node enforces no egress and the scheduler places nothing that sets it there, saying why; the capacity is declared, not read, because a virtual machine's link speed is not what its provider sold |

> Corrected 2026-10-21, when an endpoint's own bandwidth was built: one row said every bandwidth
> limit needed a declared uplink. Only egress does; an ingress limit and an endpoint's own are
> held on the workload's link, so the node holds them wherever it can write the qdiscs (§4.1 of
> `04-runtimes.md`). An egress budget's cut-off needs only nftables, and its floor `tc`; a node
> without `tc` holds a spent budget's floor as a cut-off (§5.1 of `05-scheduling.md`).

**A node that lacks something offers less; it does not fail to start, and it never silently offers
what it cannot enforce.** Missing KVM removes two isolation tiers from what the scheduler will
place there (§4.1 of `04-runtimes.md`, capabilities are declared, never inferred). Missing Landlock
removes the tenant file API. This is the same discipline as everything else: the capability is
advertised or it is not, and nothing in between.

Sizing is ordinary and deliberately not prescribed, a node's capacity is whatever `allocatable`
says after reservations, and §5.2 of `05-scheduling.md` already makes that an honest number. The
one non-obvious figure is memory for `microvm` tiers, where per-workload overhead is real and is
the reason hibernation (§6 of `22-first-party.md`) exists.

---

## 3. Enrolling a node

The agent dials out (§3 of `02-architecture.md`), so enrollment opens nothing:

```
korpis node token create fra-3 --label region=eu --expires 1h
→ korpis-agent join --server https://korpis.example:7443 --token korpis_8f2a… \
      --ca-fingerprint sha256:4c1a…
```

The join token is single-use and short-lived. The agent presents it once, receives a per-node key
pinned to the control plane's certificate, and never uses the join token again. The operator's
firewall requires no inbound rule, NAT is irrelevant, and a node behind a residential connection is
an ordinary node rather than an unsupported configuration.

What the token is, precisely. Creating one needs `node.enroll` over the organization the node will
belong to. The token is an ordinary Token subject holding a grant of `node.enroll` over that
organization, attenuated from the creator's, with `max_uses: 1` and an expiry of one hour unless
the operator asks for less or more, and never more than a day: it is a bearer credential for a
machine. The node's name and labels are bound to the token when it is created, so the node an
agent enrolls is the one the operator described, whatever the agent claims. The answer carries the
fingerprint of the installation's certificate authority, and the join command carries it to the
agent, so the agent's first connection is verified against it rather than trusted. The secret is
shown once and never stored (§6 of `10-api.md`).

The agent sends the token with the public half of an Ed25519 key it generated (§7 of
`02-architecture.md`), over a connection it verifies before trusting: the control plane presents
the installation CA behind its own certificate, and the agent proceeds only if that CA has the
fingerprint the join command carries and the certificate verifies under it. It keeps that CA and
from then on trusts it alone. The key is written to the agent's state directory, readable by the
agent's user only, before it is ever sent, so a join whose answer was lost is retried with the
same key; the node's identifier is written last, so a join that stopped half way is not mistaken
for an enrollment. The control plane redeems the token's one use online (§6.2 of
`08-identity.md`), records the node and its key in one transaction, and returns the keys that sign
assignments and capability tokens. An agent whose answer was lost asks again with the same token
and the same key and is told the same node; a different key is refused. A token whose use was
spent by a failed enrollment, a name taken meanwhile, is not given back: the operator creates
another, which is the cost of a counter that can never be exceeded.

Once joined, `korpis-agent run` is the node: it holds the control stream, keeps the journal,
asks for leases, and reconciles, and it starts whether or not the control plane is reachable,
because a node keeps doing its job from the set it holds (§4.6 of `02-architecture.md`). It is
told which driver to use and how much of the machine to keep back for the machine's own system
(§2.1 of `05-scheduling.md`). Stopping it, for an upgrade or by `SIGTERM`, leaves every workload
exactly as it is and writes what the journal still counts in memory; the next start adopts them
(§4.4 of `02-architecture.md`). Its drivers are processes reached on `--driver-socket`,
repeatable, one for each driver the node runs (`/run/korpis/driver-oci.sock` unless told
otherwise), each asked its name and capabilities as the agent starts and reported to the control
plane, and the helper on `--helper-socket` (`/run/korpis/helper.sock`); `--driver memory` runs
nothing, for trying the agent alone. The
node's content store is under its state directory, in `content`, and a development registry spoken
to over plain HTTP is named with `--plain-http`. A recipe's `fetch`, and a pull from a registry a
recipe names, reach only the public internet (§4 of `09-recipes.md`, §3 of `17-security.md`); a
mirror on a private network is named with `--fetch-allow`, a CIDR, repeatable, which the control
plane takes too for the registries it resolves recipes from. What the node offers for inbound
traffic is `--advertise-address`, repeatable, for an address clients reach it at that it cannot see
itself (a router forwarding to it, or a LAN address), and `--ports`, the range its endpoints' ports
are allocated from (30000-32767 by default); both are measured before anything relies on them (§9
of `07-networking.md`). A node serves the ingress unless its agent runs with `--ingress false`: the
helper opens ports 80 and 443 on the advertised addresses and starts `korpis-ingress`, from beside
its own binary or where `--ingress-binary` says (§3.2 of `07-networking.md`).

The container tier is a separate process, `korpis-driver-oci`, run as its own user with access to
containerd's socket and to nothing else the agent holds (§6.1 of `02-architecture.md`). It serves
the driver protocol on `/run/korpis/driver-oci.sock`, to the users named by `--allow-uid` and, when
none is named, to its own user only; it reads images from the node's content store at
`/var/lib/korpis-agent/content`, which the agent fills, and mounts nothing but directories under
`/var/lib/korpis/volumes`, which the helper keeps, and each workload's own identity directory under
`/var/lib/korpis-agent/identity`, which the agent keeps at mode `0750` (§6 of
`02-architecture.md`); `--socket`, `--content`, `--volume-root`, `--identity-root`, and
`--containerd` move them, and an agent given another state directory needs the driver told the
same identity root. Stopping
the driver stops nothing that runs: the containers are containerd's, and the next driver reads
them back with `Recover`.

The `microvm` tier is another process, `korpis-driver-firecracker`, which serves the driver
protocol on `/run/korpis/driver-firecracker.sock` to the users `--allow-uid` names, and which the
agent is given as a second `--driver-socket`. It runs as root, because Firecracker's jailer must:
the jailer makes each guest's chroot, cgroup, and pid namespace and then runs that guest's
Firecracker as the workload's first id, so no guest runs as root. What does beside it is the
guest's console writer, as a container's does under its shim, moved into the guest's cgroup. The
driver reads Firecracker, the jailer, the guest kernel, and `korpis-guest` from `/usr/lib/korpis`
(`--firecracker`, `--jailer`, `--kernel`, `--guest`), and starts `korpis-stream-writer` and
`korpis-image-build` from beside its own binary (`--stream-writer`, `--image-build`); it reads
images from the node's content store as the `oci` driver does, and volumes from beneath the
volume root; it keeps its objects, the drives guests share, and their jails in
`/var/lib/korpis-driver-firecracker` (`--state-dir`), and each guest's cgroup beneath
`korpis-firecracker` (`--cgroup`). Stopping it stops no guest and no console, and the next one
reads the guests back with `Recover`. How a node is given the kernel and Firecracker is question 7
of §10.

The privileged helper runs only when asked. Its socket is a systemd socket unit
(`ListenStream=/run/korpis/helper.sock`, `SocketMode=0660`, the supervisor's group as
`SocketGroup`), and its service starts `korpis-helper --inherit` with `StandardInput=socket`, so
the helper receives the listening socket, serves whoever connects, and exits after a minute without
a connection. It serves only the users `--allow-uid` names, root alone when none is named, keeps
volumes in the `local` class under `--volume-root` (`/var/lib/korpis/volumes` by default) and its
record of id ranges under `--state` (`/var/lib/korpis/helper`). Where systemd is absent, `--socket
PATH` binds the socket itself and serves until stopped. The class's backend is read from the
filesystem under `--volume-root`: ext4 made with `-O quota,project` or XFS, mounted with
`prjquota`, makes `local` a project class with byte and inode limits in the write path (§3 of
`06-storage.md`); btrfs with qgroups enabled makes it a class of one subvolume per volume, its
bytes limited by its qgroup and its inodes not at all (§3 there); any other filesystem makes it a
`directory` class, which enforces no size and says so (§2 of `06-storage.md`). Corrected
2026-10-19: this paragraph named only the project classes after the btrfs class was built, and so
told an operator on btrfs that their volumes would not be limited. The jobs it starts are binaries
beside it unless named: `--job-binary` for backups and restores and `--install-binary` for installs
(`none` runs no installs); each job's cgroup is made under `--job-cgroups`
(`/sys/fs/cgroup/korpis`), and `--content` names the agent's content store, which restores and
installs read (`/var/lib/korpis-agent/content`).

Removing a node is the reverse and is never abrupt: cordon, drain, decommission (§3 of
`05-scheduling.md`), each producing a Plan that names every workload that will move and where it
will land. Drain arrives with migration, in Phase 3 (§4 of `20-roadmap.md`); until then, emptying a
node is cordoning it and stopping or deleting what runs there. Removing it is then `korpis node
remove` (`NodeService.RemoveNode`, the irreversible action `node.remove`), refused unless the node
is cordoned and holds no workload and no volume not deleted, since nothing yet moves a volume off a
node: the node is decommissioned for good, its session ends on whichever replica holds it, and its
key opens no other. A node removed as compromised (`--compromised`) is removed whatever it holds,
which §9 of `17-security.md` describes; the CLI asks for `--yes` either way. Added 2026-10-16, when
it was built; the documents named decommissioning from the start and no operation reached it.

What the node made on its machine is not removed with the node: a node that is removed has stopped
acting, so whatever it had not destroyed itself, which is everything a node removed as compromised
ran, stays until the operator removes it. Its containers are containerd's, in the `korpis`
namespace, and its volumes are the helper's, under its volume root; its workloads' networks, each a
namespace, a routed link, and nftables rules in the `inet korpis` table, are released by
`korpis-helper release-networks [--netns-root PATH]`, which releases every network bound under the
root as the node releases one when it destroys its workload, prints each, and fails naming any it
could not. Added 2026-10-19, when `scripts/e2e-phase0.sh` was found to leave every workload's
network on the machine it ran on, and nothing the operator could run removed one.

**Enrolling a relay** that runs as its own process (§3.1 of `02-architecture.md`) is the same
procedure with three differences. The token is created over the root organization only
(`NodeService.CreateRelayEnrollmentToken`), because a relay carries every organization's sessions,
and it binds the relay's name and the URL clients and nodes will reach it at. The relay joins with
`korpis-server relay join --server … --token … --ca-fingerprint …`, presenting the public halves of
two keys it generated and kept before sending anything: an Ed25519 identity key it signs its
requests to the control plane with, and a P-256 TLS key. And it is given a certificate for its
URL's host, issued by the installation's authority, which agents and the CLI already trust, so no
trust has to be distributed for it. The certificate lives ninety days, shortened per relay by up to
nine so relays that joined together do not expire together (§7). The relay renews it itself at
about two thirds of its life, shifted by up to a day by its identifier: it makes a fresh TLS key,
asks with `RelayControlService.Renew` signed by its identity key over that key, writes the new key,
chain, and expiry as a generation of their own, points its directory at it, and serves it from the
next handshake without a restart. A renewal that stops half way leaves the generation before in
service. The authority's issuing is an effect of its own (`relay.certificate_renew`, by
`system:authority`). `ListRelays` shows each relay with its certificate's expiry and whether it is
connected, and `RemoveRelay` removes one: it is named to no node and told nothing more, and the
certificate it holds stays valid until it expires, since the installation does not revoke
certificates. A joined relay runs with `korpis-server relay run`, listening by default on every
IPv4 address at its URL's port (`--listen` overrides it), and stops on its own once the control
plane refuses its feed. Behind a load balancer, both `relay run` and `serve` take
`--proxy-protocol-from` with the balancer's addresses, which must then write a PROXY header on
every connection (§3.1 of `02-architecture.md`). Where relays carry the sessions, `korpis-server
serve --relay-role false` takes the role off the control plane, so nodes attach to the relays
alone.

SFTP is served by a relay an operator turns it on for (§3.1 of `02-architecture.md`):
`--sftp-listen ADDRESS:PORT` on `relay run`, or on `serve` for the control plane's own relay
role, with `--sftp-host-key PATH` naming the host key, made there at the first start where the
file is absent and readable by the server alone, and `--sftp-advertise HOST:PORT` where clients
reach it at another address than it listens on, as behind a load balancer or a port forward. The
relay reports the address and the key's fingerprint in its feed, and `ServerInfo` lists them, so
the panel and `korpis sftp` tell a person where to connect and which key to expect. Several relays
behind one name are given the same key file, or a client is shown a key it has never seen on its
next connection; a key replaced is a warning every client then shows, which is the warning a
changed host key is meant to give. Turning it on extends the relay the trust §9.9 of
`08-identity.md` states, and it is the operator's to extend, relay by relay; `--proxy-protocol-from`
applies to the SSH port as to the relay's own, since `source_cidr` is judged by the client's address
there too.

> Built 2026-10-22. Without `--sftp-advertise`, a relay advertises its URL's host and the port
> `--sftp-listen` bound, and the control plane its `--public-url`'s host and that port; a control
> plane with neither refuses to start, since clients could not be told where to connect. The host
> key is `sftp_host_key` in the relay's directory, or the control plane's data directory, unless
> `--sftp-host-key` names another. The address and the key travel in the feed request the relay's
> identity key signs, so no one between them can send clients elsewhere or vouch for another key;
> a relay opens its feed again as soon as it starts serving SFTP. The control plane records each
> change as `relay.sftp_reported`, or `relay.sftp_withdrawn` when a relay stops, so a replaced
> host key is on the record before any client warns of it, and `ServerInfo` lists every admitted
> relay's endpoint, refreshed with the relays named to nodes.


---

## 4. Upgrade

This is where the incumbents break. Pterodactyl's extension ecosystem patches core files with
`sed`, so an upgrade is a coin flip; its transfers deadlock into states requiring manual SQL; and
its agent upgrade restarts tenant workloads.

Four rules, each of which is a property of the architecture rather than a procedure to remember:

**Control plane first, then agents, one at a time.** The agent-protocol window of §4 of `10-api.md`
covers every protocol version shipped by a supported release, every LTS included, so the control
plane always speaks the version every deployed agent speaks.

**A mixed-version fleet is a supported steady state.** Not a window to rush through. An operator
who upgrades the control plane on Monday and gets to the last three nodes in a fortnight is
operating normally, which is what makes cautious upgrading possible for someone who has other work.

**Upgrading an agent does not restart tenant workloads.** This is the single most valuable
operational property in the design and it is why `Recover` is mandatory rather than optional in §4
of `04-runtimes.md`. The agent stops, the new binary starts, re-adopts the running workloads by
their `workload_id` / `intent_id` / `assignment_generation` / `lease_epoch` labels (§4.4 of
`02-architecture.md`), reconciles, and continues. A game server with forty players on it does not
notice that its supervisor was replaced. The agent's other executables follow the same rule and
their own skew rules (§6.2 of `02-architecture.md`).

**Migrations are forward-only, online, and reversible for one version.** No long-held locks, no
schema change that takes a table offline, and every migration accompanied by the inverse needed to
roll the binary back within the support window. The `Intent` chain is immutable and append-only
(§3.1 of `03-state.md`), so rolling *configuration* back is a different and easier operation: it is
re-declaring an earlier intent version, which produces a Plan and converges. Bet 3's payoff.

> As built, 2026-10-17 (`crates/korpis-store/src/schema.rs`, `korpis-server migrate`). Every
> migration after 0074, the baseline every installation starts from, is a pair: what it applies
> and its inverse, and the check refuses one that comes without the other. Each inverse is tested
> to leave exactly the schema the migrations before it made (columns, constraints, indexes,
> views, functions, triggers, privileges, and the owner's memberships, compared with a database
> migrated only that far), and undoing to the baseline and applying again on a populated store is
> tested to keep every row. `korpis-server migrate` applies what its build carries; `--to <n>`
> undoes, newest first, what was applied after `n`, so the binary that ran before can run again;
> `--dry-run` says which without doing it. It runs as the schema's owner, named by
> `--database-owner-url` or read from `/etc/korpis/owner.env`, where installation keeps it. It
> refuses, before changing anything, to undo past the baseline, a migration its build has no
> inverse of, or one its build does not carry, which only the build that applied it can undo:
> sqlx's own undo passes over such a migration silently and would leave a schema the older binary
> was not built for. `serve` reads which migrations are applied before it does anything else and
> refuses a schema it was not built for: an older one, to be migrated by this binary, or a newer
> one, to be served by the binary that applied it or undone by that binary's `migrate --to`.
> Rolling back is the operator's to order: the newer binary undoes, then the older binary
> starts; no binary undoes on its own. What is not built yet: a migration is not checked for
> long-held locks, which "online" above asks of every migration, and is reviewed for it instead.

> As built, 2026-10-18 (`crates/korpis-app/src/node/session.rs` and
> `crates/korpis-agent/src/session`). A control plane refuses an agent whose protocol is outside
> its window of §4 of `10-api.md` and says which side to upgrade: an agent newer than the window
> means the control plane was not upgraded first; one older than it is from a release no longer
> supported, and is the one to upgrade. The agent tells that refusal apart from every other end of
> its session, by the refusal's reason rather than its words, logs it as an error with what the
> control plane said, and dials again after five minutes rather than within seconds, since only an
> upgrade mends it. `scripts/e2e-phase0.sh` upgrades a running node's agent as a package does, the
> installed binary replaced by a rename and the agent restarted, and checks that the workload kept
> its process and was adopted, not started again. The binary put back there is the same build, so
> the test proves the replacement and the adoption, and the session test proves the window; two
> releases whose protocols differ have not been run against each other, since there is one protocol
> so far.

---

## 5. High availability, and what it actually covers

**Control plane.** `korpis-server` holds no state that is not in Postgres, so availability is a
matter of running several. There is **no leader**: reconciliation work is claimed from a queue with
`SELECT … FOR UPDATE SKIP LOCKED` (§6 of `03-state.md`), so N replicas share the work, a replica
that dies has its claims reclaimed when they expire, and adding capacity is adding a process. The
few genuinely singleton operations (schema migration, advancing the `max_issued_epoch` watermark)
take a Postgres advisory lock and are brief.

Avoiding leader election here is deliberate. Leader election is where distributed systems acquire
their most interesting failure modes, and this workload does not need it.

**A connection pooler changes how the job queue wakes up.**

> External review, finding R14. Recorded in §16 of `23-walkthroughs.md`.

§6 of `03-state.md` wakes workers with `LISTEN`/`NOTIFY`, which does not survive PgBouncer in
transaction-pooling mode: a `LISTEN` issued on a pooled connection is not attached to the session
that later receives the notification. Nothing breaks loudly. The queue silently falls back to
polling, latency goes from milliseconds to the poll interval, and nobody connects the two events.

So it is stated rather than discovered: with a transaction-mode pooler in the path, workers poll,
the interval is configurable, and `korpis-server` **detects the pooling mode at startup and logs
which wake-up path it is using**. Session-mode pooling keeps notifications. Either is supported;
being unsure which one is running is not.

**Postgres.** Korpis does not implement database high availability. Patroni, a managed service, or
accepting single-instance risk are all valid; Korpis states the requirement, synchronous durability
for the transaction that writes an `Effect`, and leaves the rest to tools built for it.

**Nodes.** A node is a single point of failure for the workloads on it, and no amount of
control-plane availability changes that. What Korpis provides is the ability to *express* the
answer: replicas, anti-affinity across nodes and failure domains, a replicated storage class, and a
`stable` endpoint whose public address survives the move and the loss of any one node or edge (§3
of `07-networking.md`). A `node_bound` address does not survive the loss of the node it is anchored
to, and is described that way wherever it is shown. What it does
not provide is automatic recovery of stateful workloads whose only copy of their data was on the
failed node, because that is not recoverable, and implying otherwise would be the kind of claim P4
forbids.

**Edges.** `stable` and `ingress` endpoints (§3 of `07-networking.md`) concentrate many workloads'
reachability into a forwarding tier, which reintroduces at the network layer exactly the shared
failure domain the control plane design removed. **This is the cost of the headline feature and it
must be stated rather than discovered:** in Pterodactyl a node's failure takes down that node's
servers and nothing else, because the address lives on the node. With `stable`, an edge's failure
takes down every endpoint behind it, including workloads that are running perfectly on healthy
nodes.

So an edge is never a single machine in any deployment that cares:

| Approach | Failover | Demands |
|---|---|---|
| VRRP / floating address | seconds, within one L2 segment | simplest; single-site only |
| ECMP with equal-cost routes | sub-second, per-flow rehash | a router that speaks it |
| BGP anycast | route withdrawal | an ASN, transit that permits it, real network operations |

Every one of those mechanisms fails over an edge that is **known** to have failed, and knowing is
the part that was missing: §3.1 of `07-networking.md` now gives an edge a lease with an epoch and
measures its liveness **through the forwarding path** rather than from the health of the process.
Failover advances the epoch. Without that, all three rows above answer a question nobody was
asking, because a half-failed edge on a healthy host was not detected at all (Finding 21 of
`23-walkthroughs.md`).

Forwarding state is derivable from placements, so a replacement edge reconstructs it from the
control plane rather than needing state replication, an edge holds no truth of its own. On one
machine there is no edge tier and no `stable` mode; endpoints are `node_bound`, which on one machine
behaves identically because nothing can move. The single-machine constraint of §1 is not violated
by an option that only exists at scale, and the option is not pretended into existence where it
cannot hold.

**Relays.** A relay holds no state of its own: it verifies by what its feed sends, and the
sessions it carries live only as long as their streams (§3.1 of `02-architecture.md`). So
availability is a matter of running several, and every node keeps a data connection to each, so
any relay a client reaches can carry a session with any connected node. Losing a relay ends the
sessions it was carrying; a client opens the next through another relay `ServerInfo` lists, and
the streams it reads resume from their last offset (§2 of `14-streams.md`). Nothing has to fail
over, because no relay is primary. What a relay depends on is its feed: with the control plane
unreachable it keeps verifying by the last of each it received, which is the outage behaviour
§4.6 of `02-architecture.md` states, and it renews its certificate only when the control plane
is back, which the third of the certificate's life left at renewal makes a wide margin. Several
relays behind one balancer are reached by one address; the balancer's health check of a relay is
a TCP connection, or with PROXY enabled a header of the `LOCAL` command, which the relay treats
as the balancer's own connection. On one machine the control plane's own relay role is the only
relay, and its availability is the control plane's.

**The control plane can be entirely absent and workloads keep running.** §4.6 of
`02-architecture.md` guarantees it, and it is the reason disaster recovery here is a recovery
rather than an outage: agents hold their assignments and their leases, and a lease's `on_expiry`
policy is `keep_running` by default for every workload whose storage no other node can attach (the
derived default of §4.5 of `02-architecture.md`). Workloads on storage another node could attach
default to `stop`, by design, because for them an unrenewable lease is a reason to stop.

---

## 6. Disaster recovery

Two different things, frequently conflated:

**Tenant data** (volumes, snapshots, backups) is `06-storage.md`, content-addressed, deduplicated,
client-side encrypted, restorable to a new volume.

**Korpis itself** is this section, and it is small:

| Must be backed up | Why |
|---|---|
| the Postgres database | all intents, grants, effects, placements |
| the control plane's grant signing key | grants and capability tokens verify against it; rotated on restore |
| the assignment-signing keyring | every public key that ever signed an assignment, verify-only, so a restore can recover assignments the store lost (§8 of `03-state.md`) |
| the node identity certificate | agents' per-node keys are pinned to it, and it is **not** rotated on restore |
| the control plane's backup key | it seals every `operator`-custody repository's key-encryption key (§5.5 of `06-storage.md`); without it no tenant backup made under it can be read, so it is **not** rotated on restore |
| the control plane's TLS key | it seals the private key of every certificate the ingress serves (§3.2 of `07-networking.md`); lost, every `acme` certificate is issued again with a new key and every `provided` one must be uploaded again, so it is worth keeping, not essential |
| the control plane's factor key | it seals every account's TOTP secret (§9.6 of `08-identity.md`); lost, no TOTP factor opens, so each account signs in with a passkey, or with its password and a recovery code, and enrols TOTP again, and an account whose password had only TOTP and spent recovery codes beside it is recovered by an administrator (§9.3), so it is worth keeping, not essential |
| the installation's ACME account key | it is the account `acme` certificates are ordered with; lost, a new account is made at the next order, and certificates the old one ordered stay valid until they expire, so it is worth keeping, not essential |
| the `max_issued_epoch` watermark and the epoch issue-rate cap | the first fence mechanism of §8 of `03-state.md` computes its bound from both |

**The backup lives outside the system it protects.**

> Finding 15 of `23-walkthroughs.md`.

Nothing above forbids writing that Postgres dump into a Korpis `Repository`, and it is the obvious
thing to do, because that is the backup system already in front of the operator. It is also
circular: the repository is client-side encrypted against a key whose reference lives in the
database being backed up, so the restore path needs the thing it is restoring.

So the control plane's own backup target is **declared outside Korpis**, the preflight of §2
refuses an install that has not named one, and its encryption key is material the operator holds
rather than a reference the database resolves. This is the one place in the design where "use our
own primitives" is the wrong answer, and it is worth the exception because the alternative is
discovering it during a disaster.

> As built, 2026-10-18 (`crates/korpis-server/src/control_backup.rs`). `korpis-server backup`
> writes every row of the table above but the database as one file in the target installation
> declared: every file under the data directory's `keys` and `tls` (the grant signing key, the
> assignment keys, the CA nodes pin, the backup, TLS, and factor keys, the ACME account), never its
> `backup` directory, and the epoch watermark with its rate cap, read from the database. The file
> is sealed with XChaCha20-Poly1305 under the key installation printed once, written beside itself
> and renamed into place, read back and opened before the command returns, and the newest 30 are
> kept (`--keep`). Installation writes a unit that runs it daily and a timer to start; it runs as
> the control plane's own user, which owns the data directory and to which installation gives the
> target, never as root, so that nothing a compromised control plane could write into its data
> directory (a link, or another target) steers what root reads or where it writes. Corrected the
> same day, from a review of the first version, which ran it as root. It reads no link, only the
> files the data directory holds. The database is PostgreSQL's own to back up: the command asks
> whether `archive_mode` is on and whether the last archiving attempt failed, and when the database
> is not being archived it fails, after writing the keys and saying it did, so the gap shows in the
> unit's state rather than in a disaster. Korpis does not configure archiving itself, which is the
> database's own configuration (§2), and does not check that a base backup exists, which PostgreSQL
> does not record. The only target is a directory; restoring from the file is the procedure below.

**Which key is rotated, and which is not.**

> Finding 16 of `23-walkthroughs.md`.

Step 2 below rotates the **grant signing key**. It does not rotate the **node identity
certificate** that §3 pins at enrollment. §6 of `08-identity.md` states the separation and the
reason; the consequence for this section is that a restore ends with agents that reconnect, rather
than with an operator re-enrolling every node by hand in the middle of the incident the restore was
fixing.

The restore procedure has **three** steps that cannot be skipped, and §8 of `03-state.md` makes all
three mandatory rather than documented. The first two are the same principle applied to the two
kinds of authority the control plane hands out: *a restored control plane must invalidate authority
it can no longer account for.* The third is the same principle applied to configuration: *a
restored control plane must not silently forget what it had decided.*

> **1. Advance the lease epoch past every epoch that could have been issued after the backup.**

> Second external review, finding S9. Recorded in §17 of `23-walkthroughs.md`. This step used to say
> "advance the lease epoch past every epoch it may have issued **before** the backup was taken",
> relying on "the monotonic watermark". That is the arithmetic finding R11 corrected in §8 of
> `03-state.md`: the watermark is in the database being restored, so it is as stale as everything
> else in it, and the epochs that matter are the ones issued *after* the backup. This section had
> not been updated with the correction.

A backup is by definition older than the failure, so every epoch issued between the backup and the
failure is larger than anything the backup records, and a restored control plane that resumed from
the backup's watermark would hand out an epoch a live agent already holds. Two mechanisms close it,
both from §8 of `03-state.md`: the epoch issue rate is capped, so `watermark + cap × backup age` is
a real upper bound that the restore advances past; and a settling window, during which nothing is
issued, lets reconnecting agents present the signed epochs they hold so the floor rises past the
highest one seen.

> **2. Rotate the signing key.**

Capability tokens verify **offline** against that key (§6 of `08-identity.md`), which is what lets
console access survive an outage, and it means a token's validity does not depend on the database
that recorded it. Restore an older backup and a grant issued *and revoked* after it is absent from
the database while its token still verifies: authority in circulation, no record of it, and an
audit trail that contradicts reality. Rotation invalidates every token issued after the backup.
Sessions re-authenticate, which is a bounded and visible interruption during a disaster the
operator is already handling, and the short token lifetimes of §6 of `08-identity.md` bound how
long it lasts.

The same step holds the relays that run as their own process (§3.1 of `02-architecture.md`). A
relay's identity key is the authority that obtains its feed, and removing a relay revokes it; a
backup older than the removal restores the relay as joined, and a relay sees the plaintext of the
sessions it carries (§8 of `17-security.md`), so a relay removed for being compromised would return
unnoticed. So the restore holds every relay the backup records as joined: its feed is answered as
unavailable, which it retries, and it is named to no node, until an operator readmits it
(`NodeService.ReadmitRelay`) or removes it. A relay that joined after the backup is unknown to the
restored control plane: its feed is refused, it stops, and it joins again with a new enrollment
token. Holding rather than rotating follows the reasoning that leaves node identities unrotated:
re-enrolling every relay during the incident lengthens it, while readmitting one is one call. The
cost is that until a relay is readmitted, sessions open only through the control plane's own relay
role, and not at all where `--relay-role false` turned that off.

> As built, 2026-10-18 (`crates/korpis-app/src/restore.rs`, `crates/korpis-app/src/relay.rs`).
> Holding marks every relay not removed with `held_at`, recorded as an effect naming the operator.
> A held relay that proves itself is answered `UNAVAILABLE`, which it retries at most thirty
> seconds apart, so a readmitted relay is fed again without a visit to it; a feed already open
> when it is held ends. Its certificate is not renewed while it is held, by the same check. It is
> left out of the relays nodes are named, and `ListRelays` shows when it was held. `ReadmitRelay`
> clears the mark and is refused for a relay that is not held, or removed; removing a held relay
> works as for any other. A relay the restored store does not know is refused as unauthenticated,
> and stops. What a held relay does not lose is what it was fed before the restore: it still
> verifies data connections by the token keys it last held, so it is isolated by being named to no
> node and to no client, not by anything it is told.

> **3. Recover the assignments the store no longer has, from the agents running them.**

During the same settling window, agents present their signed assignments. Intents, workloads,
power declarations, and in-flight plans that the restored store lacks are recovered from them with
their original identifiers, and every workload's version counter is advanced past the highest
version presented, so the fleet is not rolled back to the backup's configuration without a plan,
and no version number is issued twice (§8 of `03-state.md`). Acknowledged journal records the
agents still retain are re-ingested, refilling the effect log from the backup's cursor onward.

Skipping any step converts a recovery into an incident. None is a procedure someone can forget; all
three run before the restored control plane serves a request.

> As built, 2026-10-18 (`crates/korpis-server/src/restore.rs`). `korpis-server restore` runs at the
> machine while the control plane is stopped, after PostgreSQL has restored the database by its own
> means, with `--from` the backup file and `--key` (or `--key-file`) the key installation printed.
> It runs as the data directory's owner, the control plane's own user, and refuses to run as anyone
> else, as the backup does, so that nothing a compromised control plane could put in its data
> directory steers what a more privileged user writes; and it writes through no link. It opens the
> file, which another key does not, and puts its keys and certificates back into the data directory:
> a file missing is written as installation wrote it; a file present with the same bytes is kept; a
> file present with other bytes stops the restore before anything is written, since only the
> operator knows whether the backup's or the one present is right. A data directory with no backup
> declaration, as a machine restored from nothing has, is given one: the file's directory as the
> target and the key it was opened with. It refuses a schema this binary was not built for, as
> `serve` does. Then the three steps that run without the agents: the epochs fenced from the file's
> watermark and rate, with the settling window (`--settle-secs`, ten minutes by default) left
> pending, since a window opened while the control plane is stopped could run out before it starts;
> the token key rotated; the relays held; and `restore.gap` written. Each is safe to run again, so a
> restore stopped part way is finished by running it again. The control plane's first start opens
> the pending window, and while it is open every request that needs a caller is answered
> `UNAVAILABLE`, so no one acts on the backup's state or declares a version a node is running before
> what the agents present is recovered; the agents' sessions, the relays' feeds, and what needs no
> caller (`ServerInfo`, signing in) are served. What it does not check: that the control plane is
> stopped, which a second replica left running would undo by issuing epochs before the fence; and
> that the database is the one the file was taken with, which only the operator knows. Running it as
> the owner rather than as root was decided the same day, from a review of the first version.
>
> The whole of it is exercised end to end with a real agent
> (`crates/korpis-agent/tests/agent/restore.rs`): a control plane backed up by `korpis-server
> backup`, then changed (a workload declared that the backup never knew, and a new intent for a
> workload it held), both reaching the node; then stopped, its older database put back, the
> restore run, and the control plane started with the agent running. While the window is open a
> caller is answered `UNAVAILABLE`, and the agent's presentation recovers the new workload where it
> runs (`bound_by` recovery, not placed a second time), both intents with their counters, and the
> plans applying them; nothing is held, since the node presented everything placed on it. Once
> the window ends the control plane serves what the node runs, and answers the node with a set
> naming both.

What is genuinely lost is stated rather than implied away: declarations made after the backup and
never approved reached no node, and control-plane effects with no node counterpart in that interval
cannot be recovered. The restore writes a gap record for the interval. Continuous WAL archiving
keeps the interval short; the procedure keeps it honest. The earlier claim that the fleet
re-converges "from state that was never lost" was true of authority and false of configuration until
step 3 existed.

---

## 7. Certificates and rotation

The classic self-inflicted outage in this class of system is an internal certificate expiring and
every node disconnecting within the same hour, at 03:00, on a holiday.

- Agent keys and control-plane certificates rotate automatically, well before expiry.
- Lifetimes are **staggered at issue**, so a fleet enrolled on one afternoon does not expire on one
  afternoon.
- Approaching expiry is an event with escalating severity (§5 of `15-observability.md`), not a log
  line nobody reads. As built for relays: a relay certificate fourteen days from expiry is a
  `relay.certificate_expiring` warning, three days from it critical, each reported once, and a
  renewal starts the count again; the control plane looks hourly. As built for the ingress's
  certificates, measured in each one's own life since they range from days to years: five sixths
  through it is a `certificate.expiring` warning, seventeen eighteenths critical, reported to the
  certificate's organization once each per validity (§3.2 of `07-networking.md`).
- Manual rotation exists and is a documented, rehearsed operation, because it is what a compromise
  requires (§9 of `17-security.md`).

The ingress's `acme` certificates are ordered from the directory `--acme-directory` names, Let's
Encrypt's production directory by default (§3.2 of `07-networking.md`). The installation's account
there is made at the first order, never at start, and only if the operator agreed to the
directory's terms with `--acme-agree-terms`: without it every `acme` certificate stays pending and
says why. `--acme-contact mailto:…`, repeatable, gives the account a contact, which Let's Encrypt
writes to about expiring certificates and changes to its terms. The account's key is kept in the
data directory beside the other keys, one file per directory, so moving to another directory makes
another account. A directory with an authority of its own, a test one or an enterprise one, is
trusted with `--acme-ca`, a PEM certificate, instead of the system's roots.

A portal's `acme` certificate is ordered from the same directory with the same account (§3.6 of
`07-networking.md`), designed 2026-10-21. Where no DNS provider of its organization serves the
name's zone, the directory validates it by TLS-ALPN-01 on port 443 of the name, answered by the
control plane's own listener, so an installation serving portals is reached on 443 at their names,
directly or through a load balancer that passes TCP (with `--proxy-protocol-from`, §3), never one
that terminates TLS itself, which would see the handshake the control plane must answer. Every
connection that names no portal is still served with the installation's own certificate, so agents,
relays, and clients that pin the installation's authority see no change.

---

## 8. The runbooks that must exist

Naming them is part of the design, because an operational property nobody has written down is one
that will be improvised during an incident:

| Situation | Core of the procedure |
|---|---|
| node unreachable | confirm; `stop` workloads move once their leases provably end; `keep_running` ones move only through an `evacuate` Operation that names the split risk (§3 of `05-scheduling.md`) |
| node compromised | revoke key, **increment lease epoch**, reschedule from intents, rebuild the host |
| control plane lost | restore Postgres and keys, **advance the epoch floor, rotate the signing key, hold the relays until readmitted, and recover assignments from agents** during the settling window, then serve |
| Postgres lost, no backup | unrecoverable for configuration; workloads keep running; re-declare intents |
| migration stuck | inspect phase; migration is resumable and reversible by construction (K-4, §8 of `05-scheduling.md`) |
| agent version drift | expected and supported; upgrade at leisure within the supported window (§4 of `10-api.md`) |
| certificate expiry approaching | rotate; the alarm should have fired days earlier |
| quota exhausted, tenant blocked | quota is enforced in the write path; raise it or free space, never disable enforcement |
| a node's disk filling | inode and byte quota per volume; identify, then drain (§10 of `17-security.md`) |
| an extension misbehaving | it is a workload, throttle, restart, or evict it like one |

Every one of these is a procedure a Pterodactyl operator has performed with manual SQL, and each is
listed here because the design's claim is that none of them require it.

---

## 9. Air-gapped and constrained environments

Recipes and extensions are OCI artifacts in ordinary registries (§2 of `09-recipes.md`), so an
air-gapped install mirrors a registry (a solved problem with existing tools) rather than needing a
bespoke offline bundle format. Digest pinning means a mirrored artifact is verifiably the same
artifact. A recipe's `fetch` is mirrored the same way, by any HTTP server holding the files: the
hash, not the URL, is what is checked, so the mirror's address goes in place of the original. A
mirror on a private network is named to the control plane and to every node's agent with
`--fetch-allow`, because neither reaches a private address on a recipe's behalf otherwise (§4 of
`09-recipes.md`, §3 of `17-security.md`); a mirror the operator already names as a development
registry over plain HTTP needs nothing more. Nothing in the control
plane requires outbound internet access, and telemetry is off by default (§6 of
`15-observability.md`).

---

## 10. Open questions

1. **Bundled Postgres. Resolved in §2**: `korpis-server install` provisions or connects, a compose
   quick-start exists for evaluation and says so at startup, and no database is vendored. What
   remains open is whether distribution packages arrive before or after there is a second
   maintainer to keep them from going stale (§6 of `19-governance.md`). → here
2. **Multi-region control plane. Carried by §10 of `03-state.md`, question 6.** One Postgres and
   nodes on three continents means reconciliation latency crossing oceans. Regional read replicas
   plus the `consistency_token` of §6 of `10-api.md` cover reads; whether writes need regional
   partitioning is unresolved and is a large change if so.
3. **Hardware attestation.** Carried from §13 of `17-security.md`. Measured boot would make
   compromised-node detection real; the key management is disproportionate for a single-operator
   install and would have to be optional. → here
4. **Windows node operations. Carried by §9 of `04-runtimes.md`, question 6.** Enrollment,
   preflight, upgrade-without-restart, and drain all assume a Linux process model. K-9 requires
   Windows on day one at the *interface* level; the operational surface has not been worked
   through.
5. **Upgrade rehearsal.** A `korpis upgrade --plan` that reports which agents would fall outside
   the support window, which extensions declare incompatible API versions (§6 of
   `16-extensions.md`), and which migrations would run, before anything moves. This is P5 applied
   to the platform itself and is probably not optional. → here
6. **Node pool autoscaling.** Growing a cluster in response to pressure requires a provider
   interface (create a machine, install the agent, enrol it). That is close to "Korpis is not a
   cloud provider" and needs a boundary drawn before it is built. Routed here from §10 of
   `05-scheduling.md` (question 2) and not carried until now (finding S18). The likely boundary is
   the same as for DNS: a node-pool provider extension creates machines and hands them an
   enrollment token, and Korpis never speaks a cloud API itself. → here
7. **How a node gets the guest kernel and Firecracker.** Today an operator runs
   `scripts/guest-kernel.sh` and `scripts/tools.sh` on the node, as the tests do (§5 of
   `21-stack.md`). A packaged node needs them shipped: built and pinned by the release pipeline,
   verified by digest when the package is installed, and upgraded with it, with what an upgrade
   does to guests already running stated (they keep their kernel until their next boot, §6 of
   `04-runtimes.md`). Decided when the `firecracker` driver becomes a node component. → here
