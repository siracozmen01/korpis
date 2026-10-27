# Build Order

**Status:** design **Date:** 2026-08-07 **Depends on:** every preceding document

---

## 1. This is order, not scope

Everything described in the other twenty-three documents ships. Nothing here removes anything; this
document decides only what is built before what, and why that particular sequence rather than a
more comfortable one.

The comfortable sequence is to build the visible parts first (a panel, a server list, a console)
and add the model underneath later. That sequence produced Pterodactyl's abandoned v2, and it is
worth being precise about why: **some decisions cannot be retrofitted, and building the visible
parts first is a commitment to all of them by default.**

---

## 2. Five ordering rules

**A. Anything that shapes stored history goes first.** `Intent`, `Plan`, and `Effect` determine
what the system can ever say about its own past. A system that mutated state directly for six
months cannot later produce an audit trail for those six months, and reconciliation cannot be added
to an imperative core without rewriting it, which is the abandoned-rewrite failure, exactly.

**B. Anything a third party depends on across an upgrade boundary goes early.** Once agents exist
on machines Korpis does not control, the protocol is frozen (§2 of `10-api.md`). Getting it wrong
before anyone has deployed it costs a week. Getting it wrong afterwards costs a major version and
everyone's trust.

**C. A security boundary is built before the thing it protects, never after.** Privilege separation
in the agent, kernel confinement, and grant evaluation are not hardening passes. The largest class
of Wings advisories, six of twenty-one, exists because the boundary was drawn after the feature
worked.

**E. A schema is frozen whole, even when it is implemented in parts.** Phase 0 freezes the API
schema (rule B), so every field a later phase needs must already be in it: the recursive tenancy
model, the grant with its delegation policy, the full Recipe and its config schema. A later phase
implements more of a schema; it never changes one. And an interface that later features plug into
is built before the first of them, so that nothing is written in-core and moved out afterwards: the
extension contract's core precedes the first backup target, identity provider, and step provider.

> Corrected 2026-10-12 (second external review, S57). The first version of this document froze the
> schema in Phase 0 while organizations, the Recipe, and the config schema arrived in Phases 2 and
> 4, and built S3 backup targets (Phase 1) and OIDC (Phase 2) before the extension contract they are
> providers of (Phase 4). Rule E is the rule both violated, and the phases below follow it.

**D. The second implementation of an interface is the test of the interface, and it must be the
most *different* one available.** Not the easiest second one. After the `oci` driver, the driver
interface is proven by `firecracker` (a separate kernel, no shared filesystem, no shared PID
namespace) not by `native`, which is similar enough to prove nothing. The same rule applies to the
second storage class, the second exposure mode, the second surface, and the second identity source.

Rule D sits alongside K-18 rather than against it: ship **one excellent** driver, and let the
second one's purpose be interface validation rather than coverage. Five adequate drivers is what
K-18 forbids; two excellent and maximally different ones is how the interface earns the right to be
called stable.

---

## 3. Dependency graph

An arrow means *built before*: the thing at the head needs the thing at the tail to exist.

```
  10-api: every schema, frozen whole (rules B and E)
  │   01-model's nouns; 08-identity's grant and recursive tenancy schema;
  │   09-recipes' Recipe and config schema
  │
  ├─→ 03-state ─→ 02-architecture ─→ 17-security ─→ 04-runtimes
  │   (Postgres,   (protocol, leases,  (privilege      (driver interface,
  │    I/P/E)       assignment,         separation,     `oci`, inside the
  │                 journal)            confinement)    boundary, rule C)
  ├─→ 08-identity (grant evaluation, capability tokens)
  └─→ 13-surface-cli (the reference client)                     Phase 0
        │
        ↓
  16-extensions, the contract core (process model, permissions, providers)
        │
        ├─→ 06-storage (backup targets are providers) ─┐
        ├─→ 05-scheduling        07-networking ────────┤
        ├─→ 02-architecture §3.1 (the relay) ─→ 14-streams
        └─→ 09-recipes (install DSL; step providers) ──┤
                                                       ↓
                                              11-surface-web    Phase 1
        │
        ↓
  08-identity in full (delegation, external identity as providers)
  15-observability (metering, tenant audit)                     Phase 2
        │
        ↓
  04-runtimes' second driver; 05-scheduling §8 (migration);
  07-networking §3.1 (edge failover groups, `stable`)           Phase 3
        │
        ↓
  16-extensions in full (actions, UI slots, delegated install, distribution);
  09-recipes' distribution and egg import; the chat client      Phase 4

  18-operations and 19-governance apply throughout, not after.
```

**This is build order, not reading order, and the two differ on purpose.** The `Depends on` line
at the top of each document records which documents a *reader* needs first. `03-state.md` depends
on `02-architecture.md`, because the store is argued from the protocol's needs; here the store is
built first, because the protocol writes to it. `16-extensions.md` depends on `09-recipes.md`,
because an extension is packaged as a recipe; here the contract core comes first, because the
install DSL's escape hatch is a provider. Neither is wrong; they answer different questions.

> Corrected 2026-10-12 (second external review, S70). The first version of this graph was presented
> without saying which kind of dependency it showed, and it disagreed with the document headers
> without explanation.

`10-api` is at the root because the schema is the only artifact every other component shares.
`08-identity` reaches everything because authorization is evaluated on every call in every document
above. `09-recipes` is split across three phases: its schema in Phase 0, because every intent names
a recipe and is shaped by its config schema, together with verification against the trust policy,
because every intent records what vouched for its recipe (§7 of `09-recipes.md`) and that field
cannot be added once intents exist; its install DSL in Phase 1, built on the contract core so that
its escape hatch is a provider from the start; and its distribution tooling, the signing tooling
among it, and egg import in Phase 4, with the ecosystem they serve.

---

## 4. Phases

### Phase 0: The parts that cannot be retrofitted

**Every schema, complete, and the protocol.** The full domain model of `01-model.md`, including
recursive organizations and quota sets; the grant schema with its delegation policy and conditions;
the full Recipe and its config schema, and the trust policy that admits a recipe; the agent
protocol with assignments, plans, and the node journal. Phase 0 implements only what its done-when
needs, but no later phase adds a field to a schema frozen here (rule E).

PostgreSQL with `Intent`/`Plan`/`Effect`, immutability enforced by the database. Grants,
attenuation, and capability tokens. The agent, dialling out, level-triggered, with full privilege
separation and lease fencing from the first commit. One runtime driver (`oci`), excellent, running
thin recipes. The CLI.

**No web client in Phase 0.** This will look wrong and it is the most important ordering decision
in the document. §1 of `13-surface-cli.md` makes the CLI the reference client precisely so that P2
is mechanically enforced; a web UI built while the API is still moving is how a project acquires a
privileged surface and, eventually, two disagreeing APIs. The CLI is also an order of magnitude
cheaper to build, so the model gets exercised sooner.

**Done when:** `korpis apply -f` places a workload on a node, converges it, survives an agent
restart without restarting the workload, and every action in that sequence is a row in the effect
log naming its grant, the denied ones naming instead their subject and the action that was
missing. `scripts/e2e-phase0.sh` checks exactly this, with the five binaries running as processes,
the recipe published with `oras`, and every step taken through the `korpis` CLI.

> Corrected 2026-10-13, by the script that checks it. This said that every action "including the
> denied ones" names its grant. A denial is the one case no grant authorized, so there is no grant
> to name; what §4 of `15-observability.md` records for it is its subject and the action that was
> missing, which is what an investigator needs, and that is what the script checks. The same run
> found the scheduler's bind and the lease it issues naming no grant at all, though both carry out
> an approved decision; they now name the grant of that decision (§3.3 of `01-model.md`).

That is the smallest artifact that proves the entire thesis.

### Phase 1: Usable by one operator on one machine

**The extension contract's core first**: extensions as workloads, permissions as grants declared
at install, event consumers, and the provider interfaces (§3 to §6 of `16-extensions.md`). Every
backup target, DNS provider, and step provider is a provider from the first one, so nothing is
written in-core and moved out later (rule E).

Then: the relay tier (§3.1 of `02-architecture.md`), which every console and file transfer runs
through. The web client. Durable streams and console. Volumes with kernel-enforced byte and inode
quota, snapshots, deletion, and content-defined-chunking backup with per-workload keys. The
shareable link of §6.1 of `08-identity.md`, the first test of Bet 1. The install DSL with its
built-in verbs. Scheduling: filter, score, bind, with `Explanation`; cordon; the shortlist
`FeasibilityQuery`, re-placement after a post-bind rejection, and the content locality and churn
signals (§2.1 and §2.2 of `05-scheduling.md`). Networking: `node_port` and `node_bound` exposure,
each node's reachability measured by the relay's probe (§9 of `07-networking.md`), and the default
egress policy of §6 there, with a workload's own policy applied by its node. The HTTP ingress on
one machine (§3.2 there): the node serves as its own edge for `ingress` endpoints, routing by name
to their workloads, with certificates the control plane obtains by ACME and renews, the domains an
organization holds and verifies, and the provided certificates it uploads; designed 2026-10-15
(§3.2 there), with what it leaves to later phases (HTTP/3 and the rest of that section's last
paragraph). Every other field of an endpoint declaration is refused at declaration until the phase
that builds it, rather than accepted and shown while nothing enforces it (Rule K-3): dedicated
addresses in Phase 3, shaping and accounting in Phase 2. Added 2026-10-15, when the egress default
was built and the declaration was found to accept what no node enforced; §3.2 of `07-networking.md`
calls the ingress table-stakes for the `service` shape, and without it the web app of this phase's
own scenario could not be reached by name. Rate limits, with the web client's login and link
redemption that are the first endpoints to need them: the two buckets and the unauthenticated
limits of §10 of `10-api.md`, and the recording of unauthenticated failures beside the denials
Phase 0 already coalesces. Workloads of lifecycle `task` and `scheduled`, with their runs and
`RunPolicy` (§4 of `05-scheduling.md`), after backups, so that a nightly backup is a scheduled run
rather than a command someone has to type; added 2026-10-14 by Finding 33 of `23-walkthroughs.md`,
since no phase had named them.

**Done when:** the single-machine constraint of §1 of `18-operations.md` is real, install, run
workloads, back them up, restore them, and read an honest console, with every displayed limit
kernel-enforced (K-3).


**Service discovery is named in Phase 1, not deferred to Phase 3.**

> External review, finding R19. Recorded in §16 of `23-walkthroughs.md`.

"A web app and its database" is a Phase 1 scenario, and one workload reaching another by a name
that survives the other's migration is a core requirement rather than a networking detail. The
*shape* of the answer touches the `Endpoint` model, the overlay, and the recipe schema at once, so
deciding it late means changing an API schema that Phase 0 froze.

What Phase 1 owes is the decision and the schema, which is small: whether dependencies are declared
on the intent or discovered through a name service, and what a name resolves to while its target is
between placements. What can wait for Phase 3 is the distributed implementation, because on one
machine the answer is a local resolver and the semantics are the same.

> Corrected 2026-10-13, during the Phase 0 spec sync. The decision was already made (§5 of
> `07-networking.md`: dependencies declared on the intent), so the schema was Phase 0's under rule
> E, and leaving it to Phase 1 would have been the very change to a frozen schema this paragraph
> warns against. `IntentBody.dependencies` and the node's measured `overlay` are now in the Phase 0
> schema. What Phase 1 owes is resolving a dependency and injecting its address; until then a
> declaration naming one is refused as not yet available, rather than stored as a promise nothing
> keeps. Built 2026-10-15, for one node (§5 of `07-networking.md`). The overlay itself, which a
> dependency across nodes needs, is built in Phase 3 with the other machinery of workloads that
> span nodes; until then every node measures `unavailable`, and a workload and what it depends on
> are placed together.

### Phase 2: Multi-tenant, and the two bets that live there

Organizations and recursive delegation, and suspending one (§2.1 of `08-identity.md`). SFTP, in
front of the same filesystem worker and file operations as the panel's file manager (§6 of
`02-architecture.md`, §3.2 of `06-storage.md`): added 2026-10-16, when the file manager was
designed for Phase 1 and SFTP was found to belong to no phase; its users are the tenants who reach
their files without a shell, who arrive here, and where its connections are accepted and how a
client names the workload are decided when it is built (decided 2026-10-22: at a relay an operator
turns it on for, the SSH user naming the workload by its identifier, with an SSH key registered to
the person, §3.1 of `02-architecture.md` and §9.9 of `08-identity.md`; built 2026-10-22, and run
against OpenSSH's own client on a real node, trace 50 of `23-walkthroughs.md`). `GrantTemplate`.
The panel's `Layout`, a landing screen a host composes for its customers (question 2 of §10 of
`11-surface-web.md`), phased here 2026-10-16 with the hosts it serves. The control plane answering
under a name an organization holds, so that a reseller's customers reach the panel, sign in, and
hold passkeys at the reseller's own address with the reseller's sign-in page (Finding 66 of
`23-walkthroughs.md`): which names, with which certificates, and how a passkey registered at one
site is offered at another (designed 2026-10-21 as portals, §3.6 of `07-networking.md` and §9.8 of
`08-identity.md`: a name the organization holds, served with a certificate of its own, and a
passkey registered at one address never offered at another, which §9.8 argues). `IdentityBinding`
and role-to-template mapping. Quota sets. Metering, with the exactness guarantees of §2 of
`15-observability.md`. Tenant-visible audit. External identity through OIDC, as a provider on the
Phase 1 contract core. Bandwidth shaping, a workload's reservation and limits (§5.2 of
`05-scheduling.md`) and an endpoint's own, and per-endpoint traffic accounting in kernel counters,
which metering reads (§7 of `07-networking.md`); until then a declaration that asks for either is
refused, or, for a workload's bandwidth, left unplaced on every node whose driver does not enforce
it (Rule K-3). The ingress's remaining protocol features, which Phase 1 leaves out (§3.2 of
`07-networking.md`): WebSocket over HTTP/2, client certificates, and the PROXY protocol for a
`passthrough` workload (designed and built 2026-10-22, with trace 51 of `23-walkthroughs.md`), and
ACME's TLS-ALPN challenge at a node's ingress, which §3.2 there placed in Phase 2 and this list
left out until 2026-10-22, when it was built; a reseller's names delegated to its customers are
Phase 1's, since domains are.

**The chat client is not here.** It is a first-party extension and ships in Phase 4 (§10 of
`12-surface-discord.md`). What Phase 2 owes it is the machinery (external identity mapped to
subjects, roles mapped to grant templates) and that machinery is owed to OIDC, SAML, resellers, and
CI regardless, so no part of it is chat-specific work.

**Done when:** a reseller delegates to a sub-tenant who delegates to a customer, quota inherits
correctly, and an external role grants a person authority over one project without an account being
created anywhere.

### Phase 3: Isolation as a choice, and workloads that move

The `firecracker` driver. Rule D's validation of the driver interface. Migration, all seven phases,
resumable and reversible (K-4). Replicated storage classes, and the network classes (Ceph, NFS,
iSCSI) of §2 of `06-storage.md`, whose question 3 decides whether one class covers them, since
`shared_access` is what live migration and failover need; homed here 2026-10-19, when Phase 1
closed and §2 there was found pointing at this section for them while no phase named them. Designed
2026-10-27 (§2.1 and §2.2 there, trace 80 of `23-walkthroughs.md`): question 3 resolved as two
kinds by what a node is given, a filesystem many nodes mount, whose store each node reads from a
marker at its root, and a device one node attaches, made, attached, and fenced by a
`provider.block_storage` (§5 of `16-extensions.md`) and attached by the kernel's initiator;
replicated volumes as Korpis's own incremental send to replica nodes over the mesh, on tickets each
receiver checks, which migration's copies share; and failover, a `stop` workload placed again once
its node is silent and its lease over, onto a node holding all its data (§2.4 of
`05-scheduling.md`). This phase builds the contract, the helper's attach, and a provider of the
tests' own; the providers for Ceph RBD and an iSCSI target's interface are core extensions of §2 of
`16-extensions.md`, built through the contract in Phase 4 with the others, as the address routers
are. Built 2026-10-27: the shared class, its store read from each node's mount, its volumes the
store's, placed wherever the store is offered, and the derived `on_lease_expiry` read from the
classes at last (Finding 200 of `23-walkthroughs.md`); then failover, for workloads with no volume
or with theirs in shared classes, each made on the node it lands on by a `remake` plan the control
plane approves, which a workload placed away from a node removed as compromised had needed too
(Finding 204 there); then replicated volumes' declaration, and the helper's halves of a replica's
transfer: the kernel's send, and btrfs-progs' receive confined to the replica's directory (§2.2 of
`06-storage.md`); then the control plane's half of replication: the nodes keeping each volume's
replicas chosen, kept, dropped, and marked stale, their bytes in their nodes' allocations, each
transfer made due on the volume's interval with a ticket, every node told its part, and receives
believed only of the transfer under way (Finding 205 there); then the agents' half: each transfer
sent through the mesh to the receiving node's own mesh address, admitted there under its ticket
from the sending node's address alone, received, and journaled, and what nothing keeps released.
Edges in failover groups, which is what `stable` exposure requires to exist at all and what lets it
survive the move (§3.1 of `07-networking.md`); designed 2026-10-26 (§3.1 there, trace 78 of
`23-walkthroughs.md`) as groups of two or more of an organization's own nodes, each stable address
held by one member under a lease with an epoch and forwarded by every member to the workload's node
over GRE inside the mesh, the client's address kept, each member's liveness probed by the control
plane through its forwarding path, and the group's ingress served by its members. Built 2026-10-26,
its control plane's half: groups, edge pools, the allocation of a `stable` endpoint's address and
port at placement, the `edge` placement check, and each node's table, tested over the API; and the
nodes' forwarding, the GRE links, and the refusal, tested in network namespaces; and the leases on
the groups' addresses, each held by the member its table names, under a lease fenced as a
workload's is, and the control plane's probe of each member through its forwarding path, which
moves an address from a member that stops answering; and a routed edge pool's address, routed by
the pool's router to the member its lease was issued to; and IPv6 stable addresses, forwarded
inside the same IPv4 links; and `stable` routed at a `cluster` pool's address where no group
serves; and the probe answered through a node no control plane shares a network with; and `ingress`
through the group, each member's proxy routing every name the group serves at the group's ingress
address and reaching the workloads through the mesh (2026-10-27). The overlay (§9 of
`07-networking.md`), so a dependency can cross nodes; designed 2026-10-25 as a WireGuard mesh of
every node, each with a pool of its own (§9.1 there, trace 74 of `23-walkthroughs.md`); each node's
pool given at enrollment, the mesh's interfaces, the peers each node is told, each pair's
reachability, dependencies across the mesh with each destination's admission, the scheduler's pair
check, and each dependency's status are built, and tested end to end with two nodes each in a
network namespace of its own. Dedicated addresses (§3 there), routed to whichever node a workload
runs on; designed 2026-10-26 (§3.7 there, trace 77 of `23-walkthroughs.md`) as pools an
organization owns, an address allocated with the placement and held by translation on its node, a
`node` pool's address pinning its workload, and a `cluster` pool's moved by an address router
extension, whose contract this phase builds and tests with a router of the tests' own; the routers
that speak BGP or a provider's API are of the core extensions of §2 of `16-extensions.md`, built
through the contract in Phase 4. Built 2026-10-26: pools on one node, routed there or on its link,
and the router's contract, with an API test's own router; the router told a migration's new node at
its cutover is built with migration. HTTP/3 at the ingress, on the UDP path edges bring (§3.2
there); built 2026-10-27, at a node's own ingress and at an edge group's ingress address, for every
route the proxy terminates without client certificates. Network rules naming workloads by label (§6
there), designed and built 2026-10-26, when it was found that they decide something on one node as
on many and that the overlay was never what they waited for (Finding 182 of `23-walkthroughs.md`).
Devices, including GPU. Dedicated cores (§5.2 of `05-scheduling.md`): a `cpuset` per driver and the
scheduler's accounting of whole cores with their siblings, which the schema accepted from Phase 0
while nothing held them; homed here 2026-10-25, when it was found that no phase named them and that
a workload asking for them ran on shared cores (Finding 167 of `23-walkthroughs.md`), and refused
at declaration until then. `AssignmentDelta` (§9 of `10-api.md`), once nodes hold enough workloads
for the difference to be measured. Drain and evacuate, as `Operation`s with their policy (§3 of
`05-scheduling.md`), since draining a node is migrating its workloads off it; and the
`template_rollout` Operation there, which issues a grant template's newer version to the holders of
an older one (§3.5 of `08-identity.md`), the one Operation of Phase 2's tenancy that needs this
machinery. With edges, the count of a node's ports learns them: until then every `stable` endpoint
reserves a port of its node, which one an edge serves does not use (§3 of `07-networking.md`).
Application-level health (Rule K-16): the node runs a workload's probes and acts on their verdicts
(§5 of `04-runtimes.md`), homed here 2026-10-23, when it was found that no phase named it while
four documents described it (Finding 151 of `23-walkthroughs.md`). The `tcp` and `http` probes are
built, the `query` probe through its protocol's health provider (§5 of `16-extensions.md`), the
`heartbeat` probe through the workload's identity socket, the `exec` probe through either driver's
`Exec`, the `firecracker` driver's through the guest's agent, and the `guest_agent` probe through a
microVM's vsock channel to its guest. A microVM's `/run/korpis` reaches its guest over that
channel: its identity socket and API relay served there by its first process (trace 65 there), and
its managed files and its dependencies' files sent as copies as they change, so a heartbeat probe
runs on either tier (Finding 154 there, closed). A running microVM's file sessions, the panel's
file manager and SFTP, are served inside its guest by the file worker its boot drive carries, and a
stopped one's by the same worker in a service microVM, a guest of Korpis's own holding its volumes
(§4.4 of `04-runtimes.md`), and so are installs into a stopped microVM's volume, by the helper's
install job in such a guest (§4 of `09-recipes.md`); a block-device volume's backup is browsed and
read inside its image by such a guest holding a copy restored for it, through `korpis backup ls`
and `cat` (§5.4 of `06-storage.md`), and a path of it is restored into a new directory of the live
volume by a copy in such a guest while the workload is stopped. The driver conformance suite (§8 of
`04-runtimes.md`) is built for the lifecycle, for `Recover` after a driver's crash, a kill for
memory, and a host's restart, for every limit a driver declares enforced, and for `Exec` as it says
it runs a command, Apache-2.0 and speaking the driver protocol alone, and both drivers conform to
it, run by the program a third party starts on its own, from a fixture file, which prints each
verdict and writes the report as JSON. Rule D's validation of the driver interface is recorded in
§4.6 of `04-runtimes.md`: the second driver left the lifecycle as it was and added what a separate
kernel with its own filesystem needs, each foreseen by §4.3 there but the workload's addresses. The
suite's isolation cases, the escape attempts §8 there requires, stay in this phase and are held for
the maintainer's design (2026-10-24): until they are built, a driver's conformance says nothing of
its isolation, and the suite's report says so. Devices, including GPUs, are designed (§6 of
`05-scheduling.md`, trace 72 of `23-walkthroughs.md`): a node's are those the Container Device
Interface's files describe, held by placements, and given to a driver as edits, which the driver
reads from the files itself (Finding 169). It is built: a request is declared and counted, the
device chosen at the bind, held, freed, and restored with placements, and given by the `oci` driver
as a node bound from the host; the `firecracker` driver refuses one. It is tested with a stand-in
device of the test's own on this machine, whose GPU is never passed through, so no real GPU has
been given to a workload. Dedicated cores are designed (§5.2 of `05-scheduling.md`, trace 73 of
`23-walkthroughs.md`): whole physical cores read from the kernel's topology, chosen at the bind and
held by placements, each holding object in a `cpuset` of its own and by default a cpuset partition,
so neither another tenant nor the host runs on its cores. A node's report of its cores is built,
and so is the scheduler's half: cores chosen at the bind, held, freed, and restored with
placements, each reserving its threads; so is the node's half that keeps other workloads off them:
the agent gives the driver the cores from its own topology and tells the helper the CPUs held,
before an object holding them is made and after one is destroyed, and the helper keeps each
driver's `shared` cgroup off them and writes their exclusive CPUs, tested on a cgroup tree of the
test's own. So is the `oci` driver's: a container holding cores checked against the topology the
driver reads, given their `cpuset`, and made a partition before it runs, unless the host shares the
cores; tested on trees of the test's own and not yet on this machine's kernel. So is the
`firecracker` driver's: the jailer makes a guest's cgroup with their `cpuset` and partition before
Firecracker runs, the guest has a vCPU for each CPU, and each vCPU's thread is pinned to its own.
The meter counts each core held, preflight names what a node lacks to offer them, and a workload
holding cores is shown its cores in place of a CPU limit. Both drivers pass the conformance cases
for dedicated cores, and a workload asking for one is declared, placed, held, and freed end to end
on this machine with the host sharing the core, so a request for them is accepted when it is
declared. What is not tested here: a partition, which would take CPUs from this machine's other
services (§5.2 of `05-scheduling.md`).

> Moved here from Phase 1 on 2026-10-15, when Phase 1's scheduling was built. Phase 1 named
> "cordon and drain", and §3.4 of `01-model.md` defines draining as migrating everything off a
> node, which this phase builds: a drain in Phase 1 could move nothing, so it would have been a
> cordon under another name, shown as `draining` while nothing drained. Phase 1 builds cordon,
> which keeps new placements off a node; the code refuses to set `draining` by hand, because a
> drain is an operation, not an operator's flag. On one machine, the installation Phase 1 is
> for, there is no other node to drain to. What it costs: until this phase, emptying a node for
> maintenance is cordoning it and stopping or deleting what runs there by hand.

**Done when:** a running workload with a 40 GB volume moves between nodes, keeps its public
address, and its meter neither resets nor double-counts across the cutover.

### Phase 4: The ecosystem

The extension contract in full: actions and commands, UI slots, installation inside a delegated
tree, and distribution (§4.1, §5, §8 of `16-extensions.md`), with the core extensions of §2 built
through it and not beside it. Recipes as OCI artifacts at ecosystem scale: publishing, signing
tooling, and egg import with its three grades.

The chat client lands here, as an extension, alongside the chat-adapter provider interface.

**Done when:** game query, RCON, a DNS provider, a Steam install step, and a complete chat client
are all extensions, and removing any of them removes only itself. The chat client is the sharpest
of these: an extension that is a *full* client, not a peripheral, is the strongest available proof
that the contract is sufficient, which is exactly why it is worth building through the contract
rather than in-core, where it would be easier and would prove nothing.

This is also the phase in which Korpis becomes a credible game panel, and that is deliberate
sequencing rather than an afterthought. `22-first-party.md` explains the position: general-purpose
underneath, unmistakably excellent at game hosting on day one, with every game-facing capability
delivered through the extension contract this phase exists to prove. The generality is structural
and costs nothing; being nobody's first choice would cost everything, and a first-party set is the
cheapest possible answer to it.

Two items in that set depend on Phase 3 rather than this one, the Minecraft handshake router and
hibernation both need the edge, which is why they arrive here and not earlier.

### Phase 5: The segments nobody else can reach

Windows drivers. Confidential-computing tiers if §13 of `17-security.md` resolves in their favour.
Protocol-aware routing. Everything the earlier phases' interfaces were shaped to permit.

---

## 5. Where each bet dies

The four bets in §4 of `00-overview.md` are falsifiable claims, so each has a phase at which it is
tested and a specific observation that would kill it.

| Bet | Falsified if | Tested in |
|---|---|---|
| **1: authority without accounts** | someone cannot be given real, bounded authority without a row in a users table, falsified early by the share link, later and harder by the chat extension | Phase 1 (share link), confirmed Phase 4 |
| **2: isolation is an operator choice** | the second, maximally different driver requires changing the driver interface | Phase 3 (partially observable in Phase 0) |
| **3: the diff is a first-class object** | Plan computation is too slow for interactive use, or common operations have to bypass it | Phase 1 |
| **4: correct tenancy gives reselling free** | any feature has to be written whose purpose is reselling | Phase 2 |

Bet 3 is testable earliest and is the one whose failure would be most expensive, since every other
document assumes Plans are cheap enough to compute on every change. If Phase 1 shows they are not,
that is the moment to find out, not Phase 4.

---

## 6. Risks, ordered

**1. Capability-based authorization has no precedent in this market.** §1 of `08-identity.md`
states this up front. Every competitor uses roles, every user expects roles, and the mitigation is
a presentation layer (`GrantTemplate` rendered as something role-shaped) rather than a schema
change. If that presentation does not land, the model is right and unusable, which is the same as
wrong. *Resolves: Phase 2.*

**2. Funding.** §9 of `19-governance.md` is honest that P10 removes every revenue mechanism the
incumbents use and that projects have died in this position. Structural mitigation: Korpis operates
no infrastructure, so its costs are near zero. *Never fully resolves.*

**3. Scope against one maintainer.** Twenty-four documents is a large model. The mitigation is Rules
A–D: the expensive-to-retrofit parts are small and early, and the large parts (drivers, storage
classes, exposure modes, surfaces, extensions) are additive behind interfaces that Phase 0 and
Phase 3 prove. *Continuously managed.*

**4. Plan latency.** Bet 3's failure mode. *Resolves: Phase 1.*

**5. The protocol freezing wrong.** Rule B puts the protocol first, which means it is designed with
the least implementation experience. The conformance suite and the agent-protocol support window (§4
of `10-api.md`) bound the damage; nothing eliminates it. *Resolves: Phase 3, when the second driver
and migration stress it.*

---

## 7. What building it requires

Not a large infrastructure, but not one machine the whole way either, and the point at which a
second and third are needed is specific.

**Phases 0–1: one machine.** Everything runs as VMs on it: PostgreSQL, the control plane, three or
four agents, a Firecracker or two.

| | Minimum | Comfortable |
|---|---|---|
| CPU | 8 physical cores | 12–16 |
| RAM | 32 GB | **64 GB** |
| Disk | 1 TB NVMe | 1 TB NVMe **plus a separate second disk** |
| Virtualization | VT-x/AMD-V with **nested virt** | none |
| Kernel | 6.1 LTS or newer | 6.6+ |

Three of those are load-bearing rather than preference:

- **Nested virtualization is not negotiable.** Bet 2 (isolation strength as an operator choice) is
  untestable without actually running Firecracker and QEMU, inside VMs. Most cheap VPS products do
  not offer it, which is why the primary machine is owned hardware rather than rented.
- **RAM is the binding constraint**, because microVM density and the hibernation claim of §6 of
  `22-first-party.md` are both memory questions.
- **A separate disk is genuinely needed.** §3 of `06-storage.md` is largely about `EDQUOT` arriving
  in the write path and about inode exhaustion, and verifying either means filling a filesystem on
  purpose.

The same machine is the CI runner. The conformance suites want VMs across a kernel matrix, which is
the second reason for nested virt.

**Phase 2 onward: two more machines, in two locations.** One box stops being enough at a specific
point, for specific reasons, migration across a real network rather than a virtual bridge, lease
fencing under a real partition, an edge with a real public address, and the nftables-versus-XDP
crossover of §11.1 of `07-networking.md`, which is a measurement that means nothing taken on a
virtual bridge. Modest machines suffice; no nested virt is needed, since tier work stays on the
primary. Plus one public IPv4, an IPv6 block, a domain, and any cheap S3-compatible bucket for
backup-target behaviour.

**Later and narrow:** one GPU, rented by the hour, for the device model of §6 of `05-scheduling.md`
in Phase 3. One Windows Server in Phase 5, where §13.5 of `17-security.md` observes that the
isolation table has no honest Windows column yet, that column gets written on that machine.

**Not needed, and worth saying because the reflex is to acquire them:** a Kubernetes cluster, a
managed database, separate CI infrastructure, or a load-generation fleet. The scale assumptions in
§9 of `03-state.md` are simulable on one box, and §1 of `18-operations.md` guarantees the
single-machine path stays real precisely so this stays true.

---

## 8. Not on the critical path

These can be built at any time by anyone and block nothing: additional recipes, themes,
translations, additional exposure modes, additional storage classes, additional identity providers,
SDKs in further languages, dashboards, and every extension in §2 of `16-extensions.md` beyond the
four that validate the contract.

That list is deliberately long. It is what Rule D buys: once an interface has been proven by a
second, maximally different implementation, the third through twentieth are contributions rather
than architecture.

---

## 9. Status

| Document | State |
|---|---|
| `docs/research/evidence.md` | complete: evidence, confidence-tagged, rules K-1…K-12, and the stated basis of K-13…K-18 |
| `docs/00-overview.md` … `docs/20-roadmap.md` | complete: the core twenty-one |
| `docs/21-stack.md` | complete: Rust throughout, TypeScript in the browser, and why |
| `docs/22-first-party.md` | complete: the recipe and extension set, and what it proves |
| `docs/23-walkthroughs.md` | complete: eighty traces and their 205 findings, and two external reviews, all patched or recorded |
| `README.md`, `CONTRIBUTING.md`, `SECURITY.md` | complete: entry point, review policy, disclosure |
| `LICENSE`, `LICENSE-APACHE`, `LICENSE-AGPL`, `LICENSE-CC-BY` | complete: the split, the full texts, and what it means for a host |
| succession document (§6 of `19-governance.md`) | **not written**; blocks the first release, not a phase |
| release pipeline (§6 of `19-governance.md`, §11 of `17-security.md`) | **not built**: reproducible builds, signed artifacts recorded in a transparency log, the SBOM, and the agent package that carries the microvm tier's files (question 7 of §10 of `18-operations.md`); blocks the first release, not a phase |
| `CLAUDE.md` | complete: the rules anyone editing these documents works under |
| `ARCHITECTURE.md`, `crates/`, `web/` | Phases 0 to 2 built: twenty-six crates and the panel, about 209,000 lines of Rust and 17,100 of TypeScript, 1,262 Rust tests and 136 of the panel's, 122 migrations, and the 162 calls of the public API |

Seventy-two numbered open questions remain open, of 145 recorded. Sixteen of them are a question
restated in a second document, carried there by name or pointing on to the one that will settle
it, so fifty-six decisions are open, each assigned to the one that will settle it. They
are the design's honest edges, not omissions: an open question with an owner is a decision
deferred deliberately, and every one of them was recorded where the person who hits it will find
it.

> Recounted 2026-10-19, as Phase 1 closed, by one rule written down so the next count can repeat
> it: a question is a numbered item of a document's final open-questions section, it is open
> unless it says it is resolved, settled, answered, or withdrawn, and it is restated when its
> heading says it is carried by another document or its last arrow names another document. By that
> rule the count of Phase 0 would have been 70 open of 126 and 55 distinct, not 69 and 59: the
> earlier count had no rule written down, and no one count can be made to agree with it.
>
> Corrected 2026-10-19, in Phase 2. That count said seventy-three and fifty-seven, and was one
> short: question 7 of §10 of `08-identity.md` said it would be "settled with organization
> suspension ..., which Phase 2 builds", and the count read the word as a resolution. So the rule
> now says it plainly: an item is resolved when it says it was, in the past, and an item saying
> what will settle it later is open. Phase 1 closed with seventy-four open and fifty-eight
> distinct. Since then that question was resolved in §6.3 of `08-identity.md`, and questions 6
> and 7 of §10 of `05-scheduling.md` in §5.1 there, which leaves the seventy-one and fifty-five
> above.
>
> Recounted 2026-10-22, as Phase 2 closed, by the rule above, with how each figure of the row
> above was counted written down so the next count can repeat it: crates are the directories of
> `crates/`; lines are every `.rs` file's under it, and every `.ts` and `.tsx` file's under
> `web/src` and `web/e2e` but the generated ones; tests are the `#[test]` and `#[tokio::test]`
> attributes, and the panel's the `it(` and `test(` calls of the same files; migrations are the
> files of `crates/korpis-store/migrations` that are not an inverse; and calls are the `rpc`s of
> `korpis/v1`. The same commands give Phase 1's figures above, at its closing commit, but for its
> TypeScript, which they give as 10,572, rounded then to 10,700. Of the questions, one was
> recorded after the count above and is open, which made 142 and seventy-two; question 8 of §10 of
> `05-scheduling.md` was resolved in §2.2 there before Phase 2 was called done, being due before
> it, which leaves the seventy and fifty-four above. A question counted as restated says "carried
> by" another document; one that says "carried from" one is that question's owner.
>
> Since that count, question 7 of §10 of `18-operations.md` was recorded, open (2026-10-23, how a
> node gets the microVM guest's kernel), which made 143, seventy-one, and fifty-five; and it was
> resolved in §2 there the same day, as the `firecracker` driver became a node component, which
> makes the seventy and fifty-four above. Resolving it named the release pipeline, which §6 of
> `19-governance.md` and §11 of `17-security.md` promise and nothing tracked, as a release blocker
> beside the succession document, in the table above.

**Phase 1 is built, and its done-when holds.** `scripts/e2e-phase1.sh` runs it as an operator
would, on one machine, every step through the `korpis` CLI: the control plane installed into a
fresh database and the machine enrolled as its node, its volumes on a btrfs with qgroups on; the
backup target for a disk installed as an extension from its image; a workload's console read and
typed into; a write past its volume's 64 MiB refused by the kernel, and its cgroup's `memory.max`
the limit its status shows (K-3); its volume backed up, a file read back out of the backup, and the
backup restored into a clone whose volume holds it; and then all of it deleted, and the node
checked to have removed what it made. `scripts/e2e-phase0.sh` still holds beside it. The panel is
checked in a browser against a real control plane by the end-to-end suite `scripts/check.sh` runs.
Building Phase 1 found the specification wrong or silent in more places, as building Phase 0 did,
and each is corrected in place where it was found; the twenty-three traces written while it was
built (§19 to §41 of `23-walkthroughs.md`) record what each found.

**What Phase 1 does not do is homed.** Every refusal the code answers with "arrives in Phase N"
names a phase that names it in §4: dedicated addresses and rules naming workloads in Phase 3, an
endpoint's shaping and accounting in Phase 2, since built there and no longer refused (§7 of
`07-networking.md`). Two local volume backends of §2 of `06-storage.md` are not built: ZFS, which
the machine Phase 1 was built on could not run without loading a kernel module its operator did not
ask for, and LVM-thin. Both are built before the first release, beside the succession document,
which still blocks it; until then a ZFS dataset is offered as a `directory` class that says it
enforces nothing, and XFS on a thin volume as the XFS project class it is, without a thin volume of
its own per Korpis volume. The network classes are Phase 3's (§4). Drain is Phase 3's too, since
draining is migrating (§4, Phase 3).

> Corrected 2026-10-19, as Phase 1 closed. This section said Phase 0 was built and that what
> remained was Phase 1, and its counts were Phase 0's.

**Phase 2 is built, and its done-when holds.** `scripts/e2e-phase2.sh` runs it on one machine, as
an operator, its resellers, and a customer's staff would, every step through the `korpis` CLI or,
for a person signing in, through the calls the panel makes. The operator makes a reseller and
gives it a quota set and a credential of its own; the reseller, acting with that credential and
never root's, makes a sub-tenant and gives it a smaller set and a credential; the sub-tenant makes
its customer. The customer's server is placed within all three sets and runs; its next, which
fits the customer's set, is refused by the reseller's, the customer told no more than that a set
it may not read refused it and the reseller, planning the same declaration, told its own; the
reseller's own servers cannot take what it guaranteed the sub-tenant; and each credential reads
only beneath where it was issued (trace 52 of `23-walkthroughs.md`). Then the customer installs
`korpis-idp-oidc` against an independent OpenID Connect issuer and binds its `ops` group to an
Operator role on one project: a person the issuer signs in restarts a server there and reads
nothing elsewhere, no account row exists before or after, and once the issuer drops her from the
group she loses that authority within the binding's interval (trace 44). The twelve traces
written while it was built (§42 to §53 of `23-walkthroughs.md`, and §36 extended) record what each
found; building it found the specification wrong or silent in more places, each corrected in place
where it was found.

**What Phase 2 does not do is homed.** Every refusal the code answers with "arrives in Phase N"
names Phase 3: dedicated addresses, rules naming workloads, migration plans, and moving a placed
workload (§4, Phase 3). Phase 2 was built and proven on the one machine §7 calls Phase 1's; the
two machines it lists from Phase 2 onward are for what Phase 3 measures, migration across a real
network and lease fencing under a real partition, and Phase 2 needed neither. The release
blockers stand as Phase 1 left them: the succession document, and the ZFS and LVM-thin volume
backends; the release pipeline was named one beside them on 2026-10-23, in Phase 3 (the table
above).

The model is drawn, and its first three phases are built. What remains is Phase 3.
