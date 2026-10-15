# Networking: Endpoints, Exposure, Policy, and Discovery

**Status:** design **Date:** 2026-08-07 **Depends on:** [`01-model.md`](./01-model.md),
[`05-scheduling.md`](./05-scheduling.md) **Resolves:** open question 8 of `01-model.md`
**Implements:** Rule K-3 (bandwidth, egress), Rule K-11

---

## 1. The unsolved problem

Pelican fixed nearly every surface complaint about Pterodactyl (plugins, webhooks, admin roles,
OAuth, multi-database, a rewritten UI) and **allocation system rework is still on its *planned*
list.** That single line, from Pelican's own published roadmap, is the most informative fact in
this market.

The reason is structural. Pterodactyl's `Allocation` is a bare `(ip, port)` row belonging to a
node. Everything hangs off it: the server's address, the firewall, what the customer sees, what the
billing system provisions. Replacing it means replacing the schema, so nobody who inherited it can.

What the whole category lacks as a result: ingress, automatic TLS, egress control, traffic
accounting, bandwidth limits (#1871, #3352), usage visibility (#3780), and service discovery.
Coolify and Cloudron have ingress and TLS but only for one workload shape.

And one consequence nobody names:

> **`(ip, port)` on a node ties a customer's address to a machine.** Migrating a server changes its
> address. Every player's saved entry breaks. Every DNS record, firewall rule, and billing record
> that referenced it is wrong. This is why migration is operationally dreaded even where it works,
> not because moving bytes is hard, but because the address moves with them.

Decoupling the address from the placement is the central design goal of this document. Everything
else follows from it.

---

## 2. The endpoint

```
Endpoint
  id, workload, name          "game", "rcon", "query", "http"
  protocol                    tcp | udp | http | https | quic
  container_port              int
  exposure                    Exposure
  address                     Address?        assigned; shape depends on exposure
  client_address              preserved | translated | proxy_protocol   how the workload
                                              sees the client (§3.4)
  domains                     []Domain
  tls                         TLSSpec?
  policy                      NetworkPolicy
  accounting                  bool
  shaping                     ShapingSpec?
```

One object covers a Minecraft UDP port, an RCON TCP port, and an HTTPS ingress with a certificate.
They differ in field values, not in kind (P7).

---

## 3. Exposure modes

This is where the address is either tied to a machine or freed from it.

| Mode | Address | Survives migration | Costs | Right for |
|---|---|---|---|---|
| `node_port` | node's IP + allocated port | **no** | nothing | hobby, dev |
| `node_bound` | an address on the node where the workload was first exposed, forwarded to wherever it runs now | **while that node exists**; not its loss or decommissioning | nothing before a move, one hop after | single machines, and fleets without an edge tier |
| `stable` | an address held by an edge tier with failover, forwarded to the current placement | **yes, including the loss of any one node or edge** | an edge tier with failover, or a routed address pool | game servers in production |
| `ingress` | name-based, shared address | **yes** | a proxy | HTTP/HTTPS, TLS services |
| `dedicated_ip` | an IP belonging to the workload | **yes**, if the IP is routable to any node | an IP, plus BGP or a provider API | games needing a whole IP, IP reputation |
| `overlay_only` | reachable by other workloads only | yes | an overlay | databases, internal services |
| `none` | none | none | nothing | bots, batch jobs |

`node_port` is what Pterodactyl offers, and it is kept because for a single-node install it is
correct and free. It is not the default, and the UI states its consequence, the address changes if
this workload ever moves.

**The default is `stable` where the installation can deliver it, and `node_bound` where it cannot,
and the plan says which and why.** A recipe's `exposure_default: stable` is a preference, not a
promise: on a single machine, or a fleet with no edge tier, it resolves to `node_bound`, and the
plan that creates the endpoint shows the resolved mode and what it does not survive.

> Added 2026-10-13, from the implementation: who resolves it, when, and with what. The intent
> keeps the declared mode, because it is the preference and an installation that gains an edge
> tier should be able to honour it. The scheduler resolves it for each placement, knowing whether
> an edge tier serves the workload, and judges reachability (§9) on the resolved mode, so a
> `stable` endpoint without an edge needs what `node_bound` needs: a node reachable on its own
> address. Before this the filter read the declared mode, and a node behind NAT took a `stable`
> endpoint whose address nothing could hold. No edge tier exists until Phase 3 (§4 of
> `20-roadmap.md`), so until then every `stable` endpoint is placed as `node_bound`. The endpoint
> object, which Phase 1 creates, records the resolved mode (`Endpoint.exposure` in §3.6 of
> `01-model.md`), and the plan that creates it shows it then; Phase 0 creates no endpoint, so it
> has no plan to show it in.

> Second external review, finding S39. Recorded in §17 of `23-walkthroughs.md`. The first version
> had one mode, `stable`, whose default edge was co-resident with the workload's node and forwarded
> to a migration's destination "for the drain interval" and then stopped. After that the address
> belonged to a node the workload had left, so the promise that no player edits their server list,
> repeated in §10 and in §5 of `18-operations.md`, failed one drain interval after every move and
> immediately on the node's loss. The two behaviours were real and different, so they are now two
> modes, each named for what it actually guarantees.

**`node_port` and `node_bound` on one node, as built.** Decided 2026-10-15. The control plane
allocates each such endpoint a port from the range its node advertises (§9), when it computes the
node's assignment, as it allocates the workload's id range there (§4.8 of `02-architecture.md`):
it holds every placement on the node, so it alone can keep two endpoints from one port. The port
stays the endpoint's for as long as the workload is placed on the node, including across a
change of the node's range, so a narrowed range does not move an address a player already has;
new allocations come from the range as it is. The allocation is in the assignment and in the
`Endpoint`, whose address is the node's advertised addresses with that port. The helper forwards
the port, on every address of the node, to the workload's address and declared container port by
destination translation, over the endpoint's protocol, and the source is left alone, so the
workload sees the client's own address (§3.4: `client_address: preserved`). On one node
`node_bound` behaves as `node_port`, because what distinguishes them is what happens after a move
(§10), and moves arrive with migration (§4 of `20-roadmap.md`): the endpoint records which it is,
and the plan that creates it says what each survives. What it costs: a conntrack entry per flow
(question 1 of §11), and an address that is the node's, so a node that changes its advertised
addresses changes every such endpoint's address with them.

### 3.1 `stable`: the edge

An **edge** owns stable public addresses and forwards to wherever a workload currently is. For
`stable`, edges come in **failover groups**: at least two edges sharing addresses through VRRP,
ECMP, or anycast (§5 of `18-operations.md`), or a pool of addresses routable to any node (BGP or a
provider API, §4). Preflight refuses to offer `stable` in an installation that has neither, because
an address that does not survive the loss of the machine holding it is `node_bound`, whatever it is
called.

```
                      stable address
   player ──────▶ 203.0.113.10:25565
                        │
                        │  kernel-level forwarding, target = current placement
                        ▼
                   node 7 ──migration──▶ node 12
                        │                    │
                        └──── target updated at cutover ────┘

   player's address never changes
```

The forwarding target is updated at migration cutover (§8 of `05-scheduling.md`), after the lease
epoch change. A TCP client sees a connection reset; a UDP client sees its game protocol time out
and reconnect; both reconnect to the same address and land on the new node. **A game server can
move between machines, and survive the loss of one, without a single player editing their server
list.** §10 states how long the switch takes.

This is the thing that makes migration ordinary rather than an event, and it is why
`05-scheduling.md` could treat draining a node as routine.

**What each mode costs in latency, stated before anyone feels it.**

> External review, finding R18, and second external review, finding S39.

This market's users feel five milliseconds, so the hop is part of the choice and is shown where the
choice is made:

- **`node_bound` adds no hop while the workload is where it started.** Forwarding is a kernel path
  on that node (nftables DNAT, or XDP/eBPF where the volume justifies it), a table lookup rather
  than a hop. After a migration, traffic crosses the original node to reach the new one, one hop,
  for as long as the workload stays away. A drain that decommissions the original node lists, in its
  plan, every `node_bound` address that will change, and offers converting them to `stable` first
  where the installation can.
- **`stable` costs one hop through the edge**, inbound. Return traffic goes back through the edge
  as well unless the node's network permits direct server return (§3.4), in which case replies
  leave the node directly. The cost is the price of an address that outlives any one machine, and
  the interface states it when `stable` is chosen.

The first version made the co-resident edge the default for `stable` to avoid the hop, which is
what broke the guarantee. Removing the hop and keeping the guarantee are not both available; which
one a workload gets is now a named mode rather than a side effect of where the edge happened to
run.

`node_port` remains available for the operator who wants the address on the node and accepts that
migration changes it, which is Pterodactyl's behaviour offered deliberately rather than inherited.

**An edge holds a lease, because everything else that forwards traffic does.**

> Finding 21 of `23-walkthroughs.md`.

Every other authority in this design is fenced. Agents hold leases with epochs, the control plane
fences its own restore, migration cuts over at one instant under a lease. The edge forwards for
hundreds of workloads and, as first designed, held nothing, which produced the one failure the
whole system is built to make impossible: a forwarding plane that has stopped passing traffic on a
host that is up, with an agent that is healthy, every node holding a valid lease and reporting
`running`, and a control plane whose observations are all correct while the service is down.

So:

- **An edge holds a lease with an epoch**, like any other data-plane component. Failover advances
  the epoch, which is what lets a replacement know the previous edge is no longer authoritative for
  an address, rather than inferring it.
- **Liveness is measured through the forwarding path, not from the process that owns it.** A
  synthetic connection through the same path a player takes is the only measurement that
  distinguishes "the edge process is running" from "the edge is forwarding". This is K-3 applied to
  a component that had escaped it: what is displayed is what was observed, and what was being
  observed was the wrong thing.
- **The detection interval is declared.** An operator choosing `stable`, and with it the
  concentrated failure mode §11 admits to (question 2), is entitled to a number for how long the
  concentration lasts before it is noticed. §5 of `18-operations.md` carries the failover
  mechanisms; this is the detection that has to precede any of them.

Dependents of a failed edge become `unsatisfiable` with the edge named, the same treatment §7 of
`05-scheduling.md` gives every other unsatisfiable condition, which is also what makes a hibernated
workload behind a dead edge visible: its wake trigger lives in the edge, so an edge that fails
silently would leave it in a state that is accurate and unreachable at once.

**When there is nothing to forward to.**

> Finding 5 of `23-walkthroughs.md`.

The address outlives the placement, which is the entire point, so the edge routinely holds an
address whose workload is stopped, hibernated, unschedulable, or on a node that has died. What it
does then is visible to every person connecting, and the three options are not equivalent:

| | What the person experiences |
|---|---|
| black hole | thirty seconds of nothing, then a timeout they blame on their own connection |
| refusal | immediate, honest, generic |
| protocol-aware status | immediate and *accurate*, only where a router understands the protocol |

**The default is immediate refusal. Never a black hole.** A timeout is P4's "unknown" rendered as a
stall, and it is the one failure mode that reliably produces a support ticket blaming the wrong
thing.

Where a protocol router is registered (§3.3), it answers with the real state (`hibernated`,
`starting`, `unreachable`, `data unavailable`) which is the same affordance §6 of
`22-first-party.md` uses to hold a player's client while a hibernated server boots. An unreachable
workload is that problem with a different message, not a different mechanism.

An edge is a role a node holds, not separate hardware: a deployment can run its edges on machines
that also run workloads, as long as there are at least two in the failover group. Forwarding is
kernel-level, which matters for latency-sensitive game traffic, and it is a tunnel rather than a
NAT, for the reason §3.4 gives.

### 3.2 `ingress`: name-based routing

For HTTP, HTTPS, and TLS-with-SNI, routing is by name and one address serves everything:

- HTTP `Host` header, HTTPS/TLS SNI
- Automatic certificates via ACME (HTTP-01, TLS-ALPN-01, or DNS-01 through a DNS provider
  interface)
- Certificate lifecycle (issue, renew, revoke) owned by the control plane, distributed to edges
- HTTP/2, HTTP/3, WebSocket, gRPC pass through

This is the part Coolify and Cloudron already do well and Pterodactyl has never had. It is
table-stakes for the `service` shape and is why Pterodactyl cannot host a web app credibly.

**UDP cannot use SNI.** There is no name in a UDP packet, so most game traffic cannot be
name-routed by anything generic. That is why `stable` exists as a distinct mode: the address is the
identity, and the edge forwards it.

### 3.3 Protocol-aware routers are extensions

Some protocols carry a hostname in their handshake. Minecraft Java sends the server address it was
asked for, and Minecraft also supports DNS `SRV`, so `play.example.com` can point at any host and
port. A router that reads that handshake could name-route many Minecraft servers behind one address
and one port.

That router is an **extension**, not core. `00-overview.md` §2 says no game appears in the core,
and a Minecraft handshake parser in the ingress path would be exactly that. Core provides the
forwarding plane and a hook; a protocol-aware router registers as a handler for a port. The
capability exists; the game does not enter the core.

### 3.4 Who the workload sees connecting

> Second external review, finding S42. Recorded in §17 of `23-walkthroughs.md`.

Game servers ban by address, rate-limit by address, and show addresses to moderators. Nothing in
the first version said what a workload sees when traffic arrives through an edge or a forwarding
node, and the obvious implementation, DNAT on the way in and SNAT on the way back, replaces every
player's address with the edge's.

**Edges and forwarding nodes tunnel; they do not translate the source.** The inbound packet is
encapsulated (WireGuard between Korpis machines, which the overlay already provides, §9) and
delivered to the workload's network namespace with its original source address. The reply follows
policy routing back into the tunnel to the edge, which sends it from the stable address. The
workload sees the real client address for TCP and UDP alike, with no cooperation from the
application. Where the node's upstream permits sending packets with the edge's address as source,
replies may leave the node directly (direct server return) and skip the return hop; many networks
filter such packets, correctly, so this is detected per node and never assumed.

**Userspace routers translate, and say so.** A protocol-aware router (§3.3) or the HTTP ingress
terminates the connection, so the workload's peer is the router. Those paths pass the client
address in the PROXY protocol (version 2) or the `Forwarded` header, and the endpoint declares
whether the workload accepts it. Where it does not, the endpoint's `client_address` is
`translated` (§3.6 of `01-model.md`), and the interface says that this workload sees the router,
not the player.

### 3.5 Records are published through a DNS provider

> Added 2026-10-13, when the DNS provider interface was built (§5 of `16-extensions.md`). Ingress
> names, `SRV` records for games, and ACME's DNS-01 challenge all needed records published, and
> nothing said in what form Korpis asks for them.

**Korpis sends an owner's records whole, and the provider makes its zones agree.** An owner is the
object the records exist for, an endpoint (`ep_…`), and the call `ApplyRecords` carries every
record that owner should have: `A`, `AAAA`, `CNAME`, `SRV`, or `TXT`, each a lowercase name, a TTL
of 30 to 86 400 seconds, and its values, at most a hundred records per owner. The provider creates
what is missing, changes what differs, and removes what the owner had and no longer has; an empty
list removes them all. It never touches a record it does not own, so it must know which are
Korpis's and whose (a TXT ownership marker beside each, the way external-dns keeps its registry,
or the provider's own tags). A name in no zone it manages fails `dns.zone_not_found`, and a name
already held by a record it does not own fails `dns.conflict`, neither retryable; an unreachable
DNS service fails retryably. Every record is checked by the control plane before any provider is
called, so all providers are asked the same well-formed thing.

The reason for whole sets rather than add and delete: the call is then idempotent, so a retry
after a lost answer, a provider replaced mid-way, or a resync after an outage all converge on the
same records, and the control plane's reconciliation, which already works from desired state,
needs nothing else. What it costs: every change resends the owner's whole set, a hundred records at
most, and every provider must keep an ownership registry, which is the price of never deleting a
record a person made. What it does not decide: which names an organization may publish. Zones are
not yet objects with owners (§4.1 of `16-extensions.md`), so a DNS provider publishes, within the
zones it manages, whatever its organization's endpoints ask for; when zones become objects, the
check moves to the control plane, before the call. A failure makes the endpoint's observation
`unsatisfiable` with the provider named, and reconciliation retries (§5 of `16-extensions.md`).

---

## 4. Addressing

**IPv6 is the default. IPv4 is the scarce resource.**

Every workload gets a routable IPv6 address where the node has IPv6. IPv4 is allocated
deliberately: shared by port under `node_port` or `stable`, shared by name under `ingress`, or
dedicated when someone pays for it.

**Inbound is closed except where declared, on both families.** A workload's addresses, IPv6 and
IPv4 alike, accept connections only on the ports of its declared endpoints, through a stateful
firewall in its network namespace; everything else inbound is dropped. A routable IPv6 address is
an address, not an invitation, and the first version, which made every workload IPv6-reachable,
never said so (finding S44).

This inverts what every panel in this market does, and it is worth doing because IPv4 is the single
largest per-workload cost a hosting operator carries. An operator who can serve IPv6-capable
clients natively and reserve IPv4 for those who need it has a materially cheaper cost structure.
Pelican added IPv6 allocations; treating IPv6 as the default rather than an option is the next
step.

```
AddressPool
  id, kind          ipv4 | ipv6
  cidr, scope       node | edge | cluster
  routing           static | bgp | provider_api
  reserved          []Address
```

`routing` determines whether a `dedicated_ip` can move between nodes. Static routing means the IP
belongs to one node and a workload holding it cannot migrate. BGP or a provider API means the IP
can follow the workload, which is what makes `dedicated_ip` compatible with migration rather than
an anchor.

### 4.1 A workload's network on a node, as built

Decided 2026-10-15, for the networking of Phase 1 (§4 of `20-roadmap.md`), which runs on one
machine; nothing here prevents the overlay of §9 from joining these namespaces later.

**The agent decides, the helper executes, the driver joins.** A workload's network namespace is
data-plane state the agent owns (§2 of `02-architecture.md`). From the assignment the agent
computes what the workload's network must be: its addresses, its declared endpoints, its allocated
ports, and its policy. The helper, which is root, makes it so with two operations, `EnsureNetwork`
and `ReleaseNetwork`, idempotent and keyed by workload, as it does for volumes (§3 of
`06-storage.md`). The driver joins the namespaces in `/run/korpis/netns/<workload>`, its network
namespace and the user namespace that owns it (below), and declares `network_modes: veth` (§4.1
and §7 of `04-runtimes.md`); it builds no networking of its own.

**One namespace per workload, routed, with no bridge.** The namespace has one interface, `eth0`,
whose peer on the host is a point-to-point link named for the workload. The host routes each
workload's addresses to its link and nothing else; inside, the default route is a link-local
gateway, `169.254.1.1` and `fe80::1`, answered by the host side. There is no shared layer-2
segment, so no workload can answer another's ARP or neighbour discovery, flood a bridge, or see a
frame not addressed to it, and every packet a workload sends crosses the host's routing and
nftables, which is where §6's policy is applied. The cost is a route and a firewall lookup per
packet that a bridge would skip for traffic between two workloads on one node, which is traffic
the default policy denies anyway.

**Addresses come from node-scope pools, allocated by the agent.** IPv4 comes from a pool private
to the node, `10.47.0.0/16` unless the operator names another with `korpis-agent run
--workload-ipv4`, because a pool that collides with the operator's own network breaks it and only
the operator knows that network. IPv6 comes from a prefix the operator routes to the node
(`--workload-ipv6`), which makes every workload's IPv6 address routable, as §4 says it should be;
a node given none uses a unique local prefix derived from its identity, with translation on the
way out, and its workloads are reachable over IPv6 only through the node's own addresses. What
leaves the node from a workload's link by any other interface is translated to the node's
address: all of IPv4, and IPv6 from a unique local address; a routed prefix's addresses are
global and need none. The rule matches the links by their names rather than the pools by their
prefixes, so it is the same for every workload and every pool, and a changed pool leaves nothing
behind in it. The agent allocates
the lowest free address of each pool when a workload first runs on the node, keeps it on disk
across restarts, releases it when the workload is retired from the node, and reports it in the
workload's observations, because an address is a fact about a node and the control plane learns
it as it learns the others.

**Inbound is closed in the namespace itself.** The namespace has its own nftables table, whose
input policy is to drop: it accepts its loopback, the replies to connections it opened, the ICMP
and neighbour-discovery messages a network needs, and new connections only to the ports of its
declared endpoints, over their protocols. It is written by the helper with the rest, so an
endpoint removed from the intent closes its port at the next assignment. A rule in the workload's
own namespace holds for traffic from anywhere, the node and its other workloads included, which
is what §4's "closed except where declared" requires.

**The host's rules live in one table, a chain per workload.** The helper keeps the host's rules in
one nftables table, `inet korpis`, with a chain per workload reached from a verdict map keyed by
its interface, and replaces a workload's chains and map entries in one transaction, so a change is
never half applied. It writes them with the `nft` command's JSON input, the interface nftables
documents as stable, rather than through a C binding, which keeps nftables' C library out of the
helper; links, addresses, and routes it makes over netlink. A node therefore needs nftables 1.0 or
later (§2 of `18-operations.md`).

What it costs. A pool that collides with the operator's network is the operator's to change, and
changing it renumbers workloads at their next start on the node. A node without a routed IPv6
prefix offers IPv6 the way it offers IPv4, translated. Per-packet accounting and shaping (§7) are
not part of this; they are added to the same chains later.

**The namespace is owned by the workload's own user namespace.** Corrected 2026-10-15, from the
implementation: the first text had the helper make a network namespace owned by the host, and a
container joining it could not start, because the kernel lets a process mount sysfs only in a
network namespace its user namespace owns, and a container's root is root only in its own user
namespace. So the helper makes the two together, a user namespace mapped to the workload's uid
range (§4.5 of `04-runtimes.md`) and the network namespace it owns, and the container joins both;
the helper maps only the range its ledger records for that workload, so no request makes it map the
host's own ids. Owning the namespace does not let the workload change it: its capabilities leave
out `NET_ADMIN`, and its seccomp profile refuses to make a namespace of its own in which it could
hold it (§2 of `04-runtimes.md`). What it costs: a kernel flaw that lets a process raise its
capabilities in its own user namespace would let it rewrite its own addresses and firewall, where a
host-owned namespace would have held, and the host's rules (§6) are what stop a rewritten address
from reaching anything. A user namespace can only be made by a process with one thread, so the
helper makes the pair in a short-lived child that waits while the helper writes its id maps and
binds it, and the helper itself still never starts a second thread (§4 of `21-stack.md`).

**What another firewall on the node does to this.** A node whose operator runs another firewall
with a forward chain that drops what it does not accept, which Docker and ufw both install,
drops every workload's traffic to and from beyond the node, because nftables lets every base
chain on a hook drop a packet whatever another accepted. The helper does not edit another
firewall: it reports such chains, and the agent logs them at start with the rule that opens each
for Korpis's links, which are all named `kw` and twelve more characters. Until the operator opens
them, workloads reach the node and the node reaches them, and nothing else does. Likewise a node
whose IPv6 forwarding is off gives its workloads IPv4 alone, and says so; the helper does not
turn it on, because doing so stops the node taking router advertisements on its own interfaces.

---

## 5. Service discovery

> Resolves open question 8 of `01-model.md`.

A web service must reach its database without knowing where it is placed or hard-coding an IP that
changes on migration.

The usual answer is cluster DNS. Korpis' primary mechanism is **explicit dependency declaration**,
with DNS as an optional convenience:

```
Intent.dependencies[]
  name       "db"
  target     WorkloadRef | EndpointRef      by ID, or by selector within the project
  inject     env | file | both
  required   bool                            block start until healthy
```

Korpis resolves each dependency at start and injects the current address:

```
KORPIS_DEP_DB_HOST, KORPIS_DEP_DB_PORT       (env)
/korpis/deps/db.json                          (file, for workloads that reload)
```

Explicit declaration is better than implicit DNS for four reasons, and they are the reasons this
model was chosen over the familiar one:

1. **The dependency graph is visible**, so the scheduler can co-locate a service with its database
   (`05-scheduling.md` affinity) instead of discovering the coupling from latency complaints.
2. **Start order is derivable.** `required: true` means the dependency must be healthy first, no
   crash-loop-until-the-database-appears.
3. **A `Plan` can show the blast radius.** "Migrating this database will reconfigure three
   dependent workloads, two of which restart" appears in the plan *before* approval (P5). Implicit
   DNS makes that impossible to compute, because nothing records who depends on whom.
4. **It works across every tier.** Environment injection reaches a container, a microVM, and a VM
   through cloud-init identically. Cluster DNS assumes a resolver the workload cooperates with,
   which a VM guest may not.

The cost is that an address change requires a restart or a reload. Mitigated by the file form for
workloads that watch for changes, and by DNS where the class supports it.

The field is in the Phase 0 schema (`IntentBody.dependencies`), so it never has to be added to a
frozen one; resolving and injecting arrive in Phase 1, and until then a declaration that names a
dependency is refused as not yet available (§4 of `20-roadmap.md`).

### 5.1 The resolver a workload uses

> Second external review, finding S45.

Every workload resolves names somewhere, and the first version said where nowhere, while the
default policy of §6 denies `169.254.169.254` (which on GCP **is** the instance's DNS resolver) and
all private ranges (where AWS puts the VPC resolver). A workload on either would have had no DNS.

- **Every workload gets the node's Korpis resolver** at a fixed link-local address inside its
  namespace (handed to VMs by DHCP and cloud-init). It answers the optional dependency names of
  this section and forwards everything else to the node's configured upstream resolvers.
- **The node, not the workload, talks to the provider's resolver.** The metadata address stays
  denied to workloads, so the credential-theft path of §6 stays closed, and resolution works on
  every cloud because the request leaves from the host.
- The resolver's address is the one exception the policy carries by default, it is visible as such,
  and it rate-limits queries per workload, because a resolver open to every tenant is a resource
  every tenant can exhaust.

**As built.** Decided 2026-10-15. The resolver's address is the workload's gateway, `169.254.1.1`,
which the node answers on every workload's link (§4.1), and every workload's `/etc/resolv.conf`
names it alone. The agent serves it: queries a workload sends to port 53 there are redirected on
the host to the agent's own unprivileged port, so the agent needs no privilege to answer them, and
it forwards each, over UDP or TCP as it came, to the resolvers in the host's `/etc/resolv.conf`,
which the host reaches, whatever they are, a local stub included. The agent knows which workload
asked by the source address, and allows each workload 50 queries a second with a burst of 200,
answering beyond that with `REFUSED`, which a resolver library treats as a failure to retry rather
than as an answer. The dependency names of §5 are answered here once dependencies are resolved
(§5). What it costs: a workload whose software ignores `/etc/resolv.conf` and asks a resolver of
its own on the public internet still reaches it, as egress allows; one that asks a private
resolver is denied by §6, as it should be.

---

## 6. Policy

No panel in this market has egress control, and it is the gap that hurts operators most, because a
compromised game server is a launchpad.

```
NetworkPolicy
  ingress   []Rule       who may connect in
  egress    []Rule       where it may connect out
  conntrack ConntrackLimits
```

**Egress defaults, applied to every workload unless deliberately changed.** This table is the
owner of the egress default. `17-security.md` §2 and §10 once described egress as "default-deny";
the default is the table below, deny for every internal destination and allow, rate-limited, for
the public internet (finding S44):

| Destination | Default | Why |
|---|---|---|
| Other tenants' workloads | **deny** | lateral movement between customers |
| Node management interfaces, SSH, the agent socket | **deny** | privilege escalation path |
| The Korpis control plane's API | **deny**, except an installed extension's workload | a workload has no business reaching it; an extension's is how it works |
| Link-local metadata (`169.254.169.254`, `fd00:ec2::254`) | **deny** | classic cloud-credential theft |
| RFC1918 / ULA generally | **deny** | the operator's own infrastructure |
| Public internet | **allow**, rate-limited | mod downloads, auth servers, updates |
| SMTP (25, 465, 587) | **deny** by default | spam is the fastest way to lose an IP range's reputation |

> Corrected 2026-10-13, while building the extension SDK. The control-plane row said **deny** for
> every workload, and an extension's workload exists to dial out to the control plane: it fetches
> its credential from its node and calls the API with it (§9.5 of `08-identity.md`), and a provider
> holds a stream there (§3.3 of `16-extensions.md`). Under the default as written no extension
> could ever work. The exception is decided by the control plane, which knows which workloads are
> installations, and carried to the node in the assignment, where the policy is applied; the node
> does not decide it. It opens the API's address alone, as the node reaches it, and nothing else of
> the control plane's. Any other workload that is granted something and wants to use it opens that
> address in its own policy, deliberately, like any other change to the default.

The metadata endpoint deny is small and specific and closes a real, repeatedly-exploited hole: a
workload that can reach `169.254.169.254` on a cloud node can often read the instance's IAM
credentials and take over the operator's entire cloud account.

**Connection tracking limits** cap concurrent connections, new connections per second, and
half-open connections per workload. Without them one workload can exhaust the node's conntrack
table and take every other workload offline, a resource exhaustion path with no visible cause, and
one that appears in practice long before anyone thinks to look for it.

**As built.** Decided 2026-10-15. Each row of the table is a rule in the workload's chain on the
host (§4.1), evaluated by the helper's nftables for every packet the workload sends:

| Row | What matches |
|---|---|
| other workloads | the node's workload pools, IPv4 and IPv6; on one node this is every other tenant's workload, and later dependencies open single addresses in it (§5) |
| node management | anything addressed to the host itself, on any of its addresses, except the resolver (§5.1) and ICMP to the gateway |
| the control plane's API | its addresses as the node resolves the name it was enrolled with, on its port; opened, alone, for an installed extension's workload, which the assignment marks |
| link-local metadata | `169.254.0.0/16` and `fe80::/10` beyond the gateway, `fd00:ec2::254` among them |
| RFC 1918 and unique local | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `100.64.0.0/10`, `fc00::/7`, and loopback |
| SMTP | TCP 25, 465, and 587, to anywhere |
| the public internet | everything else: allowed, at most 200 new connections a second with a burst of 400 |

The conntrack limits are per workload: at most 4096 tracked connections, in both directions, and
the new-connection rate above. Half-open connections have no counter of their own: the new
connection rate bounds how fast they can be made, and the node turns on SYN cookies, so a flood of
them costs the conntrack table nothing. Between two workloads on one node both ends hold (§9.1 of
`17-security.md`): the source's chain on the host and the destination's own namespace. A workload's
own `NetworkPolicy` is evaluated before the defaults. Its deny holds anywhere. Its allow holds only
for the public internet and, through a dependency (§5), for a workload it depends on: the rows that
protect the node, the operator's networks, the metadata service, other tenants, and the reputation
of the operator's addresses (SMTP) are the operator's to relax, not a tenant's, and nothing relaxes
them until an operator-level policy exists (question 7 of §11). What it costs: the limits are a
guess at what a game server or a web service needs, and one that opens more connections than that,
a crawler or a proxy, needs its policy to say so; and as §9.1 of `17-security.md` says, a node
whose root is compromised enforces none of this.

---

## 7. Accounting and shaping

Closes #3780 (no usage visibility), #1871 and #3352 (no bandwidth limit).

**Accounting** is per-endpoint byte and packet counters maintained in the kernel (nftables counters
or eBPF maps), sampled into the metering stream (§5 of `03-state.md`). It feeds the `egress`
dimension of `Quota` and is never reconstructed from averages, because it has financial
consequences (Rule K-12).

Counting in the kernel rather than in a proxy matters: it captures traffic the application never
sees or deliberately hides, and it cannot be evaded by a compromised workload.

**Shaping** uses `ResourceSpec.bandwidth` with the same reservation/limit split as everything else
(§5.2 of `05-scheduling.md`):

- reservation → guaranteed bandwidth, subtracted from the node's link capacity, never
  oversubscribed
- limit → ceiling
- the gap → burst, contended fairly under load

Implemented with HTB or an eBPF-based shaper. A workload sold "100 Mbit guaranteed, 1 Gbit burst"
outbound gets exactly that at the node's interface, and Rule K-3 means Korpis does not display the
number unless the kernel enforces it.

**Inbound has a limit and no guarantee.** A node controls what it sends and can only drop what it
receives. Congestion upstream of the node, an attack included (§8), happens before the node's kernel
sees a packet, so nothing on the node can guarantee inbound bandwidth, and Korpis does not sell one:
`bandwidth.ingress` has a `limit`, policed at the node, and no `reservation` (§5.2 of
`05-scheduling.md`). An outbound reservation, likewise, is a guarantee at the node's interface and
says nothing about the network beyond it, and the interface says so. The first version offered
reservations "per direction", which for inbound displayed a guarantee no component could hold
(finding S47).

---

## 8. Abuse and attack

`00-overview.md` §2 says Korpis is not a DDoS mitigation provider. It is, however, the component
that knows which workload is being attacked and which one is attacking, so its scope is:

| Korpis does | Korpis does not |
|---|---|
| per-source connection and packet rate limits at the edge | absorb volumetric attacks |
| SYN cookies, conntrack limits, per-endpoint thresholds | scrub traffic |
| surface attack signals as metrics and alerts | operate a global anycast network |
| null-route an endpoint on operator command or policy | decide unilaterally to drop a customer's traffic |
| provide an interface for an upstream scrubbing provider | become one |
| detect and rate-limit *outbound* abuse from a workload | none |

Outbound detection is the half operators forget. A compromised game server used as a DDoS reflector
or a spam relay costs the operator their IP reputation, and the operator usually finds out from an
abuse complaint. Egress rate limits plus outbound accounting make it visible within minutes.

---

## 9. The overlay

`overlay_only` and cross-node dependencies need node-to-node connectivity.

**WireGuard mesh**: kernel-level, encrypted, low overhead, and it tolerates nodes on ordinary
internet connections, which matters because §3 of `02-architecture.md` deliberately allows nodes
behind NAT.

**And that is exactly the limitation, stated plainly:** the agent dialling out means a node needs
no inbound port *from the control plane*. An overlay needs nodes to reach *each other*. A node
behind symmetric NAT can reach the control plane and cannot be reached by its peers.

Three positions, and Korpis takes the third:

1. NAT traversal: STUN/hole punching. Works often, fails unpredictably, and unpredictable
   networking is worse than absent networking.
2. Relay everything through the control plane: always works, makes the control plane a data-path
   bottleneck, and contradicts §4.6 of `02-architecture.md`, where the data plane survives control
   plane loss.
3. **The overlay is optional and its requirement is explicit.** Nodes that can reach each other
   join the mesh. Nodes that cannot are marked `overlay: unavailable`, and the scheduler will not
   place a workload with a cross-node dependency on them (§2.1 of `05-scheduling.md`): one whose
   dependency (§5), or whose dependent, is placed on another node.

> Correction, 2026-10-13, from the implementation. The rule also kept every workload with an
> `overlay_only` endpoint off nodes without the overlay. An `overlay_only` endpoint is reachable by
> other workloads: on its own node through the node's local network, and across nodes only
> through the overlay, so what needs the overlay is a consumer on another node, which is a
> cross-node dependency and is already in the rule. As written, the rule made the first-party
> Minecraft recipe, whose RCON endpoint is `overlay_only`, unplaceable on a single machine
> (Phase 1) and on any fleet without the overlay, although nothing on another node needed to reach
> it. The cost of the correction: an `overlay_only` endpoint placed without the overlay is
> reachable from its own node only, and the plan that creates it says so.

**Reachability is measured, and every exposure mode states what it needs.** A node cannot know
from the inside whether it is reachable, so the control plane has a relay or an edge probe each
node's advertised addresses, and the result is part of the node (`Node.reachability`, measured
by the probe below; the node's own heartbeat carries only what it advertises):

| Exposure | Requires of the node |
|---|---|
| `node_port`, `node_bound` | **inbound reachability** on the allocated ports |
| `stable` | an outbound tunnel to its edge group, which the node dials, so NAT does not matter; without an edge tier, what `node_bound` requires (§3) |
| `ingress` | the same as `stable` |
| `overlay_only` | nothing for consumers on the same node; a cross-node consumer is a dependency |
| cross-node dependencies | a working overlay (`overlay: available`) |

The scheduler filters on these (§2.1 of `05-scheduling.md`). So a hobbyist's node behind CGNAT can
host game servers **through an edge**, because the tunnel is dialled outward, and cannot host them
on its own address, because nobody can reach it. The first version said that a NAT-bound node "runs
game servers perfectly", which was true only with an edge and false without one, and no filter
existed to tell the two apart (finding S43).

> Added 2026-10-13, from the implementation: what a node is before anything has measured it. The
> probe is run by a relay or an edge, and neither exists before Phase 1 (§4 of `20-roadmap.md`),
> so a node enrolls as `edge_only` and stays so until a probe says otherwise. Unmeasured is
> treated as unreachable, because the opposite would place a workload on an address nobody may be
> able to reach and report it as served (P4). What it costs in Phase 0: no enrolled node takes a
> workload with a `node_port`, `node_bound`, or, without an edge tier, `stable` endpoint, so
> Phase 0 runs workloads without public endpoints, which its done-when does, and the tests that
> run a game server's recipe on an enrolled node stand in for the probe. Since 2026-10-14 the
> control plane runs the probe (below), and those tests wait for its measurement instead, except
> where the node is played by hand and advertises nothing.

**What a probe measures against.** A node advertises, in its Hello and in every heartbeat, the
addresses clients would reach it at and the range its `node_port` and `node_bound` endpoints'
ports are allocated from. The addresses are its operator's (`--advertise-address`, repeatable),
or, when the operator names none, the address its traffic towards the control plane leaves from,
and only if that address is routable on the internet; a node behind NAT therefore advertises
nothing of its own accord, which is the truth about it. An operator can still name a private
address, for clients on the same network, or a router's public address that forwards to the node,
and the probe decides whether that holds. The port range defaults to 30000 to 32767, below Linux's
ephemeral range (32768 and up), so an allocated port never collides with an outgoing connection's,
and above the ports services conventionally listen on; it is `--ports` on the node, and it never
reaches below 1024. A change of either makes the node's reachability unmeasured again, which is
`edge_only`, because a measurement of other addresses says nothing about these. What this does not
do: an address that changes is noticed at the next heartbeat, so for up to one heartbeat interval
the node is judged on its old addresses.

**The probe, as built.** The control plane process that holds a node's session measures it when it
has never been measured against what it advertises, and every ten minutes after. It asks the node
to listen on a random port of its range that no endpoint on the node holds, over TCP and UDP on
every address, for fifteen seconds; the node answers each 32-byte nonce sent there with its node
key's signature over a message bound to the purpose, the node, and the port, so a machine that
holds the address but not the key is not taken for the node, and the answer is good for nothing
else. The prober then tries each advertised address over TCP and over UDP, and the node is `public`
if one address answers over both, and `edge_only` otherwise. Both, because a game server's port is
usually UDP and a web endpoint's TCP, and a firewall that passes one need not pass the other; the
cost is that a node whose firewall passes only TCP is `edge_only`, although a TCP endpoint on it
would have been reachable. A measurement is recorded only if the node still advertises what was
measured. Corrected 2026-10-15, Finding 53 of `23-walkthroughs.md`: the port was any of the range,
and once endpoints hold ports of it (§3) the probe could pick one the node forwards to a workload,
so its nonces would reach the workload and the node would be judged unreachable, or reachable on
the strength of a workload that answered. The control plane holds every allocation on the node, so
it picks among the free ones, and the node refuses to listen on a port it forwards.

What the probe does not do, stated plainly. It measures from where the control plane runs. A
prober inside the node's network reaches a private address no client outside can, and a prober
on the node's own machine, which is every single-machine installation, reaches the node's public
address without crossing a firewall in front of the machine, such as a cloud provider's security
group. So on one machine `public` means the node answered at that address from inside, and the
operator still owns the outside firewall; the plan that exposes an endpoint says so. A prober
outside every node's network is what an edge tier or a relay on another machine provides (§3.1 of
`02-architecture.md`), and with one the measurement means what a client sees.

---

## 10. What changes at migration cutover

Collected here because it is the sequence that decides whether §1's central goal actually holds:

| Exposure | At cutover |
|---|---|
| `node_port` | **the address changes.** Clients must be told. This is why it is not the default |
| `node_bound` | the original node's forwarding target is updated; the address does not change while that node exists |
| `stable` | edge forwarding target is updated; the address does not change |
| `ingress` | proxy upstream is updated; the name and certificate do not change |
| `dedicated_ip` | with BGP or a provider API the IP is readvertised from the new node; with static routing the workload cannot migrate at all |
| `overlay_only` | overlay routes update; dependents are re-injected |

Every one of these is a step inside the `cutover` phase, ordered after `verify` and **after** the
lease epoch change.

**What is guaranteed, and what takes time.**

> Second external review, finding S41. Recorded in §17 of `23-walkthroughs.md`.

The first version said these updates were "atomic with the lease epoch change" and that there was
"no window in which traffic is forwarded to a node that no longer holds the lease". The rows of the
table contradict it: a BGP re-advertisement converges over seconds, a provider API moves an address
in its own time, an edge group is several machines updated one by one, and with DNAT, existing
connection-tracking entries keep sending a flow to its old target until they expire. What holds
instead:

- **No node that has lost the lease ever serves traffic.** The source stops acting under its epoch
  before the destination's is granted (§4.5 of `02-architecture.md`), so a packet still arriving at
  the source finds nothing listening and is dropped. Misdelivery to a second running copy is
  impossible; loss for a moment is not.
- **The forwarding update flushes the old flows.** Edges and forwarding nodes delete the
  connection-tracking entries for the moved address when they switch target, so UDP flows do not
  linger on the old path until a timeout.
- **The length of the window is stated in the plan**, per exposure mode: sub-second for an edge
  group, the configured convergence time for BGP, the provider's documented time for an API move.

The guarantee is single ownership at every instant. Continuity of traffic is a separate, shorter
promise, and it is now the one the document makes.

---

## 11. Open questions

1. **Edge forwarding implementation.** nftables DNAT is simple, well-understood, and adds a
   conntrack entry per flow. XDP/eBPF is faster and bypasses conntrack but is harder to write and
   debug, and its behaviour varies across kernels and NIC drivers. The crossover point is a
   measurement not yet taken. → here
2. **Edge high availability.** A single edge is a single point of failure for every `stable` and
   `ingress` endpoint behind it, reintroducing, at the network layer, exactly the fragility §4.6 of
   `02-architecture.md` removed at the control layer. VRRP, ECMP, and anycast all work and all have
   different operational demands. **Resolved in §5 of `18-operations.md`**, never a single machine;
   VRRP, ECMP, or anycast by deployment size, with state derivable from placements.
3. **IPv4 exhaustion strategies.** CGNAT, port-range sharing, and per-name sharing all extend a
   scarce IPv4 pool and all degrade some workloads. Which are offered, and how the degradation is
   surfaced to whoever is buying, is unresolved. → here
4. **Do dedicated IPs participate in quota? Resolved in §5.1 of `05-scheduling.md`**: dedicated IPv4
   addresses are a quota dimension; IPv6 addresses are not. An IP is a countable exclusive resource
   like a device (§6 of `05-scheduling.md`) and probably belongs in `QuotaSet`, but IPv4 and IPv6
   are so different in scarcity that one dimension may be misleading. It was routed to
   `05-scheduling.md`, which never carried it (finding S18).
5. **Certificate scale.** ACME rate limits are per registered domain, and a provider issuing
   certificates for thousands of customer domains will hit them. Whether Korpis needs an internal
   CA for internal names, ACME only for customer-facing ones, or an ACME account pool is
   unresolved. → here
6. **East-west policy enforcement point. Resolved in §9.1 of `17-security.md`**: east-west is
   enforced at both ends, so the destination never trusts the source; north-south is uncontained on
   a compromised node and says so; and each node's expected egress profile is published so an
   operator can enforce at a layer the node does not control.
7. **Who relaxes the protected rows of the egress default.** The rows of §6 that protect the
   node, the operator's own networks, the metadata service, other tenants, and the reputation of
   the operator's addresses are not a tenant's to open, and some workloads legitimately need one
   opened: a mail relay the operator runs, a workload that must reach a database on the
   operator's LAN. That needs an operator-level policy, and which scope it is set at (the
   installation, an organization, a node) and how a tenant sees that it applies are undecided.
   Until then nothing opens them. → here
