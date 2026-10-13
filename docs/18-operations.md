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
record, are kept regardless (§4 of `15-observability.md`).

### The binary is its own installer

> Resolves open question 1 of §10.

```
korpis-server install
```

Download one signed binary, verify it against the published digest, run it. It runs preflight, asks
where PostgreSQL should come from rather than assuming, generates the signing keys, writes the
systemd units, and prints the join command for the first node. Then it is the same binary that runs
the control plane, there is no separate installer artifact to drift out of sync with what it
installs.

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
(§4.4 of `02-architecture.md`). The driver is `oci` unless told otherwise, reached on
`--driver-socket` (`/run/korpis/driver-oci.sock`), and the helper on `--helper-socket`
(`/run/korpis/helper.sock`); `--driver memory` runs nothing, for trying the agent alone. The
node's content store is under its state directory, in `content`, and a development registry
spoken to over plain HTTP is named with `--plain-http`. What the node offers for inbound traffic
is `--advertise-address`, repeatable, for an address clients reach it at that it cannot see
itself (a router forwarding to it, or a LAN address), and `--ports`, the range its endpoints'
ports are allocated from (30000-32767 by default); both are measured before anything relies on
them (§9 of `07-networking.md`).

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

The privileged helper runs only when asked. Its socket is a systemd socket unit
(`ListenStream=/run/korpis/helper.sock`, `SocketMode=0660`, the supervisor's group as
`SocketGroup`), and its service starts `korpis-helper --inherit` with `StandardInput=socket`, so
the helper receives the listening socket, serves whoever connects, and exits after a minute
without a connection. It serves only the users `--allow-uid` names, root alone when none is
named, keeps volumes in the `local` class under `--volume-root` (`/var/lib/korpis/volumes` by
default) and its record of id ranges under `--state` (`/var/lib/korpis/helper`). Where systemd is
absent, `--socket PATH` binds the socket itself and serves until stopped. The class's backend is
read from the filesystem under `--volume-root`: ext4 made with `-O quota,project` or XFS, mounted
with `prjquota`, makes `local` a project class with byte and inode limits in the write path (§3 of
`06-storage.md`); any other filesystem makes it a `directory` class, which enforces no size and
says so (§2 of `06-storage.md`).

Removing a node is the reverse and is never abrupt: cordon, drain, decommission (§3 of
`05-scheduling.md`), each producing a Plan that names every workload that will move and where it
will land.

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
asks with `RelayControlService.Renew` signed by its identity key over that key, writes the new
key, chain, and expiry as a generation of their own, points its directory at it, and serves it
from the next handshake without a restart. A renewal that stops half way leaves the generation
before in service. The authority's issuing is an effect of its own (`relay.certificate_renew`,
by `system:authority`). `ListRelays` shows each relay with its certificate's expiry and whether it is connected, and
`RemoveRelay` removes one: it is named to no node and told nothing more, and the certificate it
holds stays valid until it expires, since the installation does not revoke certificates. A joined
relay runs with `korpis-server relay run`, listening by default on every IPv4 address at its URL's
port (`--listen` overrides it), and stops on its own once the control plane refuses its feed. Where
relays carry the sessions, `korpis-server serve --relay-role false` takes the role off the
control plane, so nodes attach to the relays alone.

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

> **3. Recover the assignments the store no longer has, from the agents running them.**

During the same settling window, agents present their signed assignments. Intents, workloads,
power declarations, and in-flight plans that the restored store lacks are recovered from them with
their original identifiers, and every workload's version counter is advanced past the highest
version presented, so the fleet is not rolled back to the backup's configuration without a plan,
and no version number is issued twice (§8 of `03-state.md`). Acknowledged journal records the
agents still retain are re-ingested, refilling the effect log from the backup's cursor onward.

Skipping any step converts a recovery into an incident. None is a procedure someone can forget; all
three run before the restored control plane serves a request.

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
  renewal starts the count again; the control plane looks hourly.
- Manual rotation exists and is a documented, rehearsed operation, because it is what a compromise
  requires (§9 of `17-security.md`).

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
artifact. Nothing in the control plane requires outbound internet access, and telemetry is off by
default (§6 of `15-observability.md`).

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
