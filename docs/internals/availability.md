# Availability

What keeps an installation serving when parts of it fail: the governing constraint, the control
plane's replicas, the database, nodes, edges and relays, certificate rotation, upgrades, and the
runbooks the design must make unnecessary of manual SQL. Installing and operating are on the [user
guide](../user/install.md).

## Contents

- [One machine, one operator](#one-machine-one-operator)
- [The control plane has no leader](#the-control-plane-has-no-leader)
- [PostgreSQL](#postgresql)
- [Nodes, edges and relays](#nodes-edges-and-relays)
- [The control plane can be absent](#the-control-plane-can-be-absent)
- [Upgrades](#upgrades)
- [Certificates and rotation](#certificates-and-rotation)
- [Runbooks](#runbooks)

## One machine, one operator

**A single operator with one machine must be able to install and run Korpis.** Korpis has a large
model; it may not have a large installation. Every feature degrades to one machine, one operator and
no cluster, and the same binaries in the same configuration model scale to a fleet: there is no
single-node mode that is a different product with a different upgrade path, which is how a project
ends up with a hobbyist edition nobody tests and a production edition nobody can afford. On one
machine there is no edge tier and no `stable` mode; endpoints are `node_bound`, which there behaves
identically, since nothing can move. An option that exists only at scale is not pretended into
existence where it cannot hold.

## The control plane has no leader

`korpis-server` holds no state outside PostgreSQL, so availability is running several. Work is
claimed from queues with `SELECT ... FOR UPDATE SKIP LOCKED`, so replicas share it, a dead replica's
claims are reclaimed when they expire, and capacity is added by adding a process. The few singleton
operations (migration, advancing the epoch watermark) take a brief advisory lock. Leader election is
where distributed systems get their most interesting failures, and this workload does not need it.

**A connection pooler changes how queues wake.** Workers wake by `LISTEN`/`NOTIFY`, which does not
survive PgBouncer's transaction pooling: a `LISTEN` on a pooled connection is not attached to the
session that later receives the notification, and the queue silently falls back to polling, latency
rising to the poll interval with nobody connecting the two. So `korpis-server` detects the pooling
mode at start and logs which wake path it uses; session pooling keeps notifications. Either is
supported; being unsure is not.

**A replica's connections are sized by concurrency, not nodes.** A node's session holds no
connection between passes, and computing its set takes one transaction. What takes connections is
work in flight: requests, placements, and passes a mesh change or resync starts for every node at
once, which wait their turn for a connection. A pass that waits past the pool's thirty seconds
(sqlx's default `acquire_timeout`) fails, and its downlink tries it again after a second, doubling
to thirty, rather than waiting for a change that may not come, so a burst larger than the pool
delays a node's set and never drops it. `--max-connections` (16) is split between transactions and
plain reads, at least two each, the change feed's listener among the reads; a transaction may wait
for a read connection and a read never waits while holding one, so a burst of transactions always
leaves the one holding the placement lock a connection to read with. Half the connections idle while
work is all of one kind, and an operator sizing PostgreSQL's `max_connections` counts every
replica's whole figure.

## PostgreSQL

Korpis does not implement database availability: Patroni, a managed service, or accepting
single-instance risk are valid, and the requirement is synchronous durability for the transaction
writing an effect. PostgreSQL alone, never SQLite or another engine, because the transactional
guarantees that make intents and effects honest are not optional; the database is the one place
Korpis cannot claim "two artifacts", and says so.

**Give it a server of its own, or bound idle transactions on a shared one.** The event log hands out
an event only once every transaction that could still commit an earlier one has ended, which
PostgreSQL knows only for the whole server ([observability](observability.md#ordering)). A session
left `idle in transaction` anywhere on the server stops every subscriber until it ends;
`idle_in_transaction_session_timeout` for other applications' roles bounds that. A working
transaction holds delivery the same (an index built, a table loaded, a `DROP DATABASE` waiting for a
checkpoint), and no timeout separates those from legitimate work, which is the strongest reason for
a server of its own.

## Nodes, edges and relays

**A node is a single point of failure for its workloads**, and no control-plane availability changes
that. What Korpis provides is a way to express the answer: replicas and anti-affinity across failure
domains, a replicated volume or one on storage another node reaches, whose workloads fail over, and
a `stable` address surviving any one node or edge. A `node_bound` address does not survive its node,
and is described so wherever shown. What it does not provide is recovery of a stateful workload
whose only copy was on the failed node, because that is not recoverable.

**Edges concentrate reachability**, which is the cost of the headline feature: in Pterodactyl a
node's failure takes down that node's servers and nothing else, because the address lives on the
node; with `stable`, an edge's failure takes down every endpoint behind it, workloads running
perfectly included. So an edge is never one machine in a deployment that cares:

| Approach | Failover | Korpis |
|---|---|---|
| a floating address | seconds, within one L2 segment | an edge pool on a shared link, the lease deciding the holder, gratuitous ARP announcing it, without VRRP's election |
| BGP or anycast | route withdrawal | an edge pool naming an address router, asked to route each address to its holder |
| ECMP | sub-second, per-flow rehash | not taken: one holder per address |

Every one fails over an edge known to have failed, and knowing is the part [edges](edges.md) adds: a
lease with an epoch and liveness measured through the forwarding path. An operator reads a failover
of about three seconds after a failure leaving the holder connected, and up to thirteen after one
that does not. Forwarding state derives from placements, so a replacement edge holds no truth of its
own.

**Relays hold no state**: they verify by what their feed sends, and their sessions last as long as
their streams. Every node keeps a data connection to each, so any relay can carry a session with any
node; losing one ends its sessions, and a client opens the next through another `ServerInfo` lists,
streams resuming at their offsets. With the control plane unreachable a relay verifies by the last
feed it received and renews its certificate when the control plane returns, a third of the
certificate's life being a wide margin. A balancer's health check of a relay is a TCP connection, or
with PROXY a `LOCAL` header. See [relays](relays.md).

## The control plane can be absent

Workloads keep running with the control plane gone: agents hold their assignments and leases, and a
lease's derived `on_lease_expiry` is `keep_running` for every workload whose storage no other node
can attach or start it from, and `stop` for those whose storage another node could, by design ([node
agent](node-agent.md)). That is why disaster recovery is a recovery rather than an outage ([control
plane restore](control-plane-restore.md)).

## Upgrades

Pterodactyl's extensions patch core files, so an upgrade is a coin flip; its transfers deadlock into
states needing manual SQL; its agent upgrade restarts tenant workloads. Four rules, each a property
of the architecture rather than a procedure to remember:

- **Control plane first, then agents, one at a time.** The agent protocol window covers every
  protocol version of a supported release, every LTS included ([API](api.md#two-surfaces)). A
  control plane refuses an agent outside its window and says which side to upgrade; the agent tells
  that refusal from any other by its reason, logs it as an error, and dials again after five
  minutes, since only an upgrade mends it.
- **A mixed-version fleet is a supported steady state**, not a window to rush: an operator who
  upgrades the control plane on Monday and the last nodes a fortnight later is operating normally.
- **Upgrading an agent does not restart tenant workloads**, which is why `Recover` is mandatory: the
  new agent adopts what runs by its labels and continues, and a game server with forty players does
  not notice its supervisor was replaced. `scripts/e2e-phase0.sh` replaces a running agent's binary
  by rename and checks the workload kept its process.
- **Migrations are forward-only, online, and reversible for one version.** Every migration after the
  0074 baseline is a pair with its inverse, refused without one, and each inverse is tested to leave
  exactly the schema the migrations before it made (columns, constraints, indexes, views, functions,
  triggers, privileges, memberships), and undoing to the baseline and applying again on a populated
  store to keep every row. `korpis-server migrate --to <n>` undoes newest first, as the schema's
  owner, refusing before changing anything to pass the baseline or undo a migration its build lacks
  or has no inverse of, which sqlx's own undo passes over silently. `serve` refuses a schema it was
  not built for, older or newer. Rolling back is the operator's to order: the newer binary undoes,
  then the older starts. Migrations are reviewed, not checked, for long-held locks. Rolling back
  configuration is easier: re-declaring an earlier intent.

## Certificates and rotation

The classic self-inflicted outage in this class of system is an internal certificate expiring and
every node disconnecting within the same hour, at 03:00, on a holiday. So agent keys and
control-plane certificates rotate automatically well before expiry; lifetimes are staggered at issue
(a relay's ninety days shortened by up to nine), so a fleet enrolled one afternoon does not expire
one afternoon; approaching expiry is an event of escalating severity, a relay certificate a
`relay.certificate_expiring` warning at fourteen days and critical at three, an ingress certificate
at five sixths and seventeen eighteenths of its own life; and manual rotation exists and is
rehearsed, because a compromise requires it.

## Runbooks

Naming them is part of the design, since a property nobody wrote down is improvised during an
incident. Every one is a procedure a Pterodactyl operator has performed with manual SQL, and none
requires it here.

| Situation | Core of the procedure |
|---|---|
| node unreachable | `stop` workloads move once their leases provably end; `keep_running` ones only through an `evacuate` naming the split risk |
| node compromised | remove it as compromised: key revoked, leases fenced, workloads placed again, certificates revoked and renewed; rebuild the host |
| control plane lost | restore PostgreSQL and the keys; advance the epoch floor, rotate the signing key, hold relays, recover assignments from agents in the settling window; then serve |
| PostgreSQL lost with no backup | configuration is unrecoverable; workloads keep running; declare intents again |
| migration stuck | inspect its phase; it is resumable and reversible by construction |
| agent version drift | expected; upgrade at leisure within the window |
| certificate expiry approaching | rotate; the alarm fired days earlier |
| quota exhausted | enforced in the write path; raise it or free space, never disable enforcement |
| a node's disk filling | per-volume byte and inode quotas; find it, then drain |
| an extension misbehaving | it is a workload: throttle, restart or evict it like one |

An air-gapped installation mirrors a registry, a solved problem, since recipes and extensions are
OCI artifacts, and digest pinning makes a mirrored artifact verifiably the same; a `fetch` is
mirrored by any HTTP server holding the files, the hash being what is checked, named with
`--fetch-allow` where it is private. Nothing in the control plane needs outbound internet access.
