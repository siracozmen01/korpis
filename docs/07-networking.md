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
  policy                      NetworkPolicy   who may connect to it, and how many (§6)
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

**A node's ports are a placement dimension.** Added 2026-10-15, when the allocation was built.
Asked what keeps a placement from landing on a node whose every port is held, the allocation itself
had no answer: it runs after the bind, when the only choices left are to fail the node's whole
assignment set or to leave the endpoint unreached. So a node's ports are counted like its memory:
`node_ports` is a reserved dimension (§2.1 of `05-scheduling.md`), a node's `allocatable` of it is
its range less one port kept free for the probe (§9), which the control plane derives from the
range the node advertises rather than taking a second figure beside it, and a placement reserves
one for each endpoint that resolves to `node_port` or `node_bound`. It is a quota dimension too
(§5.1 of `05-scheduling.md`), since a node's ports are as scarce as its addresses. The endpoints
are kept as `Endpoint` objects: each keeps its identity while its workload declares it, moving with
the workload, and gives its port back when the workload is retired, moves, or stops declaring it. A
workload's status lists them, each with where a client reaches it. What it costs: an endpoint is
counted as if no edge serves the installation, which is true of every installation until edges
arrive (§4 of `20-roadmap.md`); once one does, a `stable` endpoint it serves still reserves a port
of its node it does not use, until the count is made to know the edge, which Phase 3 does with
edges (§4 of `20-roadmap.md`); designed 2026-10-26 (§3.1), an endpoint resolved to `stable` holds
an address and port of its edge group and no port of its node. A range narrowed below what its
endpoints hold leaves an endpoint asking for a new port without one, and its status says so;
placement keeps this from happening otherwise.

**How the node forwards, as built.** Added 2026-10-15. The host's table has one map, from a port of
the node to the chain of the workload holding it, consulted for every new connection to an address
of the node, whether it arrives from outside or is made on the node itself; each workload's chain
translates its ports to its own address and the port inside it. A map keyed by the port alone is
enough because the control plane allocates a port once per node whatever its transport, and a
workload's ports are changed in one transaction with its chain, so a port is never forwarded to a
chain that is being rewritten. The workload's own table accepts new connections to the ports its
endpoints declare and nothing else, so a port it listens on without declaring is closed to
everyone, the node included. The node reports each endpoint in the workload's observation at the
address it gave the workload, so the control plane holds what the node made as well as what it
asked for. What it does not do, stated plainly: the loopback addresses are not forwarded, because a
packet from 127.0.0.1 cannot leave the host for a workload's link, so a client on the node itself
reaches an endpoint at one of the node's other addresses and not at `localhost`. And a node whose
other firewall drops forwarded traffic (§4.1) forwards nothing from outside until its operator
opens it, although connections made on the node itself still arrive.

**What the plan shows, as built.** Added 2026-10-15. The plan that creates an endpoint, or changes
its declared exposure, carries for each such endpoint the mode declared, the mode it resolves to in
this installation, and what that survives: no move for `node_port`, a move while its node exists
for `node_bound`, a move and a node's loss for the modes an edge, a proxy, a routed address, or the
overlay holds, and nothing for `none`. For an endpoint reached on a port of its node, it says too
that the node's reachability was measured by the control plane, which on one machine is inside the
node's network, so a firewall in front of the node is the operator's to open (§9). It is computed
when the plan is, from the declared endpoints and whether an edge tier serves the workload, both of
which the control plane holds; what it does not know then is the node, so it says what any node of
this installation gives, not the port.

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

> Corrected 2026-10-26 (Finding 189 of `23-walkthroughs.md`), when edges were designed: the list
> above required a synthetic connection through the forwarding path and named nothing to make it.
> The edge cannot, being what is measured; a peer edge sending through the holder sees the
> answer of a responder on its own machine delivered locally, never back through the holder; and
> the agent of the workload's node knows nothing of the edge. The control plane makes it, from
> outside every node, through each member's own address and each address it holds, to a node
> that answers with its key (below, "Liveness, measured through the forwarding path").

Dependents of a failed edge become `unsatisfiable` with the edge named, the same treatment §7 of
`05-scheduling.md` gives every other unsatisfiable condition, which is also what makes a hibernated
workload behind a dead edge visible: its wake trigger lives in the edge, so an edge that fails
silently would leave it in a state that is accurate and unreachable at once.

> Corrected 2026-10-26 (Finding 190 of `23-walkthroughs.md`), when edges were designed:
> `unsatisfiable` is a state the agent of a workload's node observes, and from which the scheduler
> places the workload elsewhere (§7 of `05-scheduling.md`). That agent cannot see an edge, and
> moving the workload does not mend it. The control plane, which probes the group, shows each
> endpoint behind it unreached with the group named, and leaves the workload's own state and its
> placement alone (below, "Where a workload may run").

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

**Edges in failover groups, as designed.** Designed 2026-10-26, for Phase 3 (§4 of
`20-roadmap.md`, trace 78 of `23-walkthroughs.md`). Each step names what decides it, where, and
with what information.

*A group, its members, and its addresses.* An edge group (§3.6 of `01-model.md`) belongs to an
organization and names two or more of that organization's own nodes as its members, for the reason
a pool's node is its organization's own (§3.7): an edge translates every connection to the
addresses it holds, so a group on another's node would take that node's traffic. Its addresses
are address pools of scope `edge` naming the group, made as every pool is, by an operator's
attestation (§3.7); an IPv4 pool, an IPv6 pool, or both. The group serves the workloads of its
organization and of those below it, the nearest group first, a workload's own organization's before
its ancestors', as a provider's name is resolved (§5 of `16-extensions.md`). A group with fewer
than two members serves nothing, since an address held by one machine is `node_bound` whatever it
is called. Korpis limits neither the members nor the addresses (P10).

*Which address and port a `stable` endpoint gets.* The control plane allocates them in the
transaction that places the workload, once the endpoint resolves to `stable` (§3), and keeps them
for as long as the workload declares the endpoint, across every move and stop: the address outlives
the placement, which is the point. Addresses are shared by port, which is what stretches IPv4
(question 3 of §11): a workload's `stable` endpoints all get one address, the lowest of the group's
pool on which each can have its declared container port, or else a port of the group's range (1024
to 65534 unless the group says otherwise); a game server's players keep typing 25565. A port is
held per transport, unlike a node's (§3), since the edge's table is keyed by address, transport,
and port: a game's TCP port and its query's UDP port share a number, as Minecraft's do. With an
IPv6 pool as well, each endpoint gets the same port at an IPv6 address too. Released when the
workload is retired or stops declaring the endpoint, an address and port rest for the pool's
quarantine before another workload is given them (§3.7). The plan says the address is not the
node's, and the endpoint shows it.

*Every member forwards the whole group; one holds each address.* Each member is told the group's
forwarding table, beside its assignments and kept on its disk as the mesh's peers are (§9.1): for
every allocated address, transport, and port, the node its workload is placed on, that workload's
address, and its container port, or no target when it is not running. So a member taking over an
address has its forwarding already in place, and only the address itself moves. Which member holds
an address is decided by the control plane under a lease with an epoch, below; the pool's routing
says how the network learns it. A `static` edge pool is on a link the members share: the holder
adds the address to the pool's interface as a host address, as a dedicated one is added (§3.7), an
address the interface had already being the operator's and never taken off (Finding 193), and
announces it with gratuitous ARP for IPv4 and an unsolicited neighbour advertisement for IPv6,
which is the floating address of VRRP's row in §5 of `18-operations.md`, without VRRP's own
election, since the lease decides. A `bgp` or `provider_api` pool names an address router, and the
control plane asks it to route the address to the holder (§3.7), which is the BGP row; the holder
adds the address to its loopback, announcing nothing, so that a port of it no endpoint holds is
refused there rather than routed back out towards the router that delivered it. (Corrected
2026-10-26, Finding 194 of `23-walkthroughs.md`: this sentence had the holder translate what
arrives without adding the address, which left every other port of it to the routing table.) One
holder at a time is the design; several members forwarding one address at once, which ECMP's row
needs, is question 14 of §11.

*The tunnel, which keeps the client's address.* Edges are members of the mesh (§9.1), since they
are nodes; a node behind NAT dials the edges it is paired with, which is why `stable` needs only an
outbound tunnel of a node (§9). The holder translates a new connection's destination, the stable
address and port, into the workload's address and port, and carries it to the workload's node over
GRE inside the mesh: one GRE link per pair of an edge and a node its group forwards to, from the
pool's last address on each side, which no workload is ever given (§4.1) and which the helper puts
on `korpis-overlay` as the node's own address in the mesh. GRE carries IPv4 and IPv6 alike in one
link, and its outer header is from the edge's own pool, which WireGuard's allowed IPs accept as
authentically the edge's, while the inner one keeps the client's address, which WireGuard alone
would refuse (§5 of `research/evidence.md`). The workload's node takes from an edge's GRE link only
connections to the ports its group forwards to that node's workloads, marks each with the edge it
came through, and routes the replies back into that edge's link by the mark; the edge reverses the
translation and replies from the stable address. Each packet arriving through a link carries the
link's mark before it is routed, and the kernel checks its source by that mark, which finds the way
back into the link; the link's arrivals are themselves routed by the main table alone, and refused
where it has no route. The helper leaves the host's own reverse-path setting as it is, strict or
loose, and both pass, where a link with no address of its own and a client's source fails either
without the mark (Finding 191). GRE to a node's mesh address that arrives other than through the
mesh is dropped before the kernel takes it, since the kernel gives a GRE packet to the link its
outer addresses name whatever interface brought it (Finding 192). So the workload sees the client's
own address over TCP and UDP (§3.4). Direct server return, detected per node and never assumed
(§3.4), is not built with the first build of edges, and replies cross the edge. What it costs: 24
bytes of each packet, so TCP's segment size is clamped to the GRE link's MTU, 1,396, as it is into
the mesh; a link per pair on each side; and a conntrack entry per flow on the holder (question 1).

*The lease.* An address held by an edge is fenced as a workload is (§4.5 of `02-architecture.md`):
the member acquires and renews it by request, its term runs from the send on the holder's side and
from the receipt on the control plane's, and a new epoch for another member is issued only once
the holder has said it released the address, having removed it and stopped forwarding it, or the
control plane's end of the term has passed. Its term is the group's, ten seconds unless the group
says otherwise and never under three, shorter than a workload's since a held address that
answers nothing is an outage and not a pause. What evaluates "two edges never hold one address":
the control plane, which writes the holder and epoch under the group's row lock and grants only by
that rule; and the member, which stops holding at its own end of the term, before the control
plane's, whatever the network did.

*Liveness, measured through the forwarding path.* The control plane probes each member of each
group, one replica at a time holding the group's probe for a while as a route is held (§3.7), and
from outside every node, as a client is, which is what makes the measurement the one Finding 21
asked for. Every member forwards a probe port of the group's, 65535 unless it says otherwise and
outside its range, on its own first advertised address and on each address it holds, over UDP, to a
node the group forwards to, through the same translation and the same GRE link a player's packet
takes. That node's agent answers a 32-byte nonce with its node key's signature over a message bound
to the purpose, the group, the address, and the node, as a reachability probe is answered (§9); so
an answer proves the packet crossed the member's translation and tunnel, and came back through
them, since the answering node routes its reply into the edge's link. The control plane sends one
probe a second to each, and a member that misses three in a row is failing; that is the declared
detection interval, three seconds unless the group sets either figure, and it is shown on the
group. A member whose own address passes is `ready`. What it does not do: a control plane on the
very machine of the node it probes through sees its reply delivered locally, so it measures the way
in and not the way back, and the control plane chooses another node where there is one; and a
control plane that reaches no member at all suspects itself, not the group, and moves nothing.

*Failover.* When the holder of an address is failing, its session has ended, or it is no longer
`ready`, and another member is, the control plane asks the holder to release and grants the next
ready member the address at the next epoch once the rule above allows it, then asks the pool's
router where there is one. A connected holder that still hears the control plane releases at once,
so failover takes the detection interval and a round trip; one that does not, the rest of its term.
Each member flushes the connection-tracking entries of an address it stops holding, and of a port
whose target changes, so a UDP flow is not left on a dead path (§10). The group's status names the
holder, the epoch, each member's last probe, and why each member is not ready.

*When nothing is behind an address.* A forwarded port whose workload is not running, and every
port of a held address no endpoint holds, is refused at once: a TCP reset or an ICMP port
unreachable, never a black hole, which is the default this section set. A protocol-aware answer
where a router is registered (§3.3) is that router's, and arrives with the routers of Phase 4.

*Where a workload may run, and what it depends on.* `stable` resolves to `stable` for a placement
where a group of two members or more serves the workload and has an address and port for each of
its `stable` endpoints; where none does and a `cluster` pool serving it has a free address, to that
address, which the workload holds and its router moves as a `dedicated_ip` workload's (§3.7), each
`stable` endpoint reached there on its own port, which is the routed pool §3.1 offers in place of a
group, at the cost of a whole address; and otherwise to `node_bound`, as before (§3). The placement
filter then asks of the candidate node what a dependency asks (§2.1 of `05-scheduling.md`): that it
reaches every member of the group through the mesh. Such an endpoint holds no port of its node, so
what the node reserves of `node_ports` counts it no more; a quota entry bounding `node_ports` still
counts it (§5.1 of `05-scheduling.md`), since admission comes before the placement that resolves it
and a quota's totals do not move when a group is made or loses a member, so an organization whose
quota bounds node ports spends one on each `stable` endpoint. A workload whose group has no ready
member, or whose address has no holder, stays running and placed where it is, and is shown with its
`stable` endpoints unreached, the group named, which the control plane knows from its own probes;
it is not `unsatisfiable`, which its node cannot observe and which would move it to no avail
(Finding 190).

*`ingress` through the group.* An `ingress` endpoint a group serves is reached at the group's
ingress address, one address of its pool kept for ports 80 and 443 over TCP and UDP, and every
member runs the ingress proxy of §3.2 with the routes and certificates of every name the group
serves; the holder translates the address's two ports to its own proxy, so the proxy sees the
client's address, and the proxy reaches each workload through the mesh as a dependent does (§9.1),
the workload's node admitting the member's own mesh address to the endpoint's port. A workload on
the holder itself is reached without a tunnel, as is any port the holder forwards to a workload of
its own. HTTP/3 is served
there, on UDP port 443 of the address, and at a node's own ingress where the node is its own edge,
which is the UDP path this section's edges bring.

What it costs, beyond the hop already stated: two machines or more doing an edge's work, whose
addresses must reach every node behind them; a GRE link per pair; a probe a second per member from
the control plane; and, for a held address whose holder dies unheard, up to one term of nothing.
What it does not do: it does not hide traffic from the members, which see it as any router does,
so a group is its organization's own nodes; it does not spread one address over several members at
once (question 14); and a member compromised can drop what it forwards, which the probe sees only
if it drops the probe too.

Built 2026-10-26, the control plane's half (P3.7b in §4 of `20-roadmap.md`): groups
(`EdgeGroupService`, `korpis edge-group`, §2 of `13-surface-cli.md`), whose members are their
organization's own nodes; `edge` pools naming a group of their own organization, on the members'
link or with a router; a `stable` endpoint's address and port allocated in the transaction that
places its workload, from the group of what it holds or else the nearest serving group with room,
a port per transport, kept across moves and released at retirement; the `edge` placement check,
and no port of the node reserved for such an endpoint; the endpoint shown at its group's address
and port; and every member told the group's table, and every node behind a member that member's
mesh address and what it may reach there, recomputed every two seconds and sent when it changes,
as the mesh's peers are. Tested over the API with two members and a node behind them. Not built
yet: the nodes' forwarding, the GRE links, the leases, the probe and failover, the refusal, the
ingress through the group, HTTP/3, and `stable` resolved to a cluster pool's address (P3.7c to
P3.7e).

Built 2026-10-26, the nodes' forwarding (P3.7c): the helper's `KeepOverlay` takes the node's mesh
address and its part in the groups' table, puts the address on `korpis-overlay`, keeps a GRE link
to each node it tunnels with and no other, and on a member translates each address, transport, and
port to its workload, marking what goes to another node into that node's link, refuses every other
packet to an address it holds with a TCP reset or an ICMP port unreachable, and puts each held
address on its interface; on the node behind, it accepts GRE from each edge's mesh address alone,
admits from each link only what that edge may reach, marks each such connection with its link, and
routes the replies back into it. The agent builds that part from the control plane's table, IPv4
alone, with a probe translation of its own first advertised address. Tested in network namespaces
with both nodes checking sources strictly and the node behind with no default route: a client
reaches the workload at the held address and port, which sees the client's own address; a port
without a target and a port no endpoint holds are refused at once; a port the node was not told to
admit is not reached; and the address is taken off when no longer held. Not built yet: the leases,
the probe and failover (P3.7d), and the rest of P3.7e, with IPv6 forwarding through the links.

Built 2026-10-26, the leases, the probe, and failover (P3.7d). The control plane names, in each
member's table, the addresses it should hold: a ready holder keeps its address; otherwise a ready
member takes it, each address going to the member a hash of the address and the member weighs
heaviest, so a group's addresses spread over its members and a member joining or leaving moves only
its own. Where no member is ready, the control plane suspects its own probe first and judges by
sessions alone: a holder still connected keeps its address, and only one whose session is down, as
its heartbeats say, or no holder at all, gives way to a connected member; so a control plane that
reaches no member through its probe still has every address held. The named member asks for the
address's lease up its session, and the control plane decides under the group's row lock, as a
workload's lease is decided: its own unreleased lease is renewed under its epoch with the group's
term, another member's is issued a new epoch, from the sequence workloads' come from, only once the
last holder released or its term passed, and a member not named is told who holds the address. The
member holds the address only while its lease is valid on its own clock, renews it a third of the
way through its term, and once the helper has taken it off says it released it, so the next member
need not wait out the term; a holder no longer named stops being renewed and so lets go, which is
how the control plane asks it to release. The helper announces an address it puts on a link with a
gratuitous ARP, and forgets, over the kernel's connection tracking, every connection to an address
it stops holding and every one to a port whose target moved, so a UDP flow follows its port at its
next datagram. Tested over the API (the member named granted, the other told who holds it, the
renewal under the same epoch, a new epoch after a release, and an address no endpoint holds
refused) and in namespaces (the announcement taking a client's stale entry, and a flow following
its port). The probe is built as designed above: one replica claims each group's probe, holding it
ten seconds and renewing it every two, and sends each interval one datagram to the probe port of
every member's own first advertised IPv4 address and of each address it holds, carrying a nonce,
the group, and the address; every member translates that port, on those addresses, to the node its
table names, and that node's agent, listening on its mesh address on each probe port an edge
forwards to it, answers with its node key's signature over the purpose, the group, the address,
itself, and the nonce. An answer verified within the interval, a second at most, passes; the
group's count of misses in a row makes a member failing, with why, and one answer makes it ready
again. A member that advertises no IPv4 address, or whose probe no node answers yet, is failing for
that reason. What changes is recorded at once, and the rest every ten seconds. Reading a group
shows each member's readiness, its last probe, and why it is not ready, and each address's holder,
epoch, and whether it answers through its holder (`korpis edge-group get`). Tested over the API,
the test answering at each member's address as the node behind it would: the member answering is
named and granted the address, the one silent is shown failing with why, and when the holder falls
silent and the other answers, the other is named, refused until the holder releases, and then
granted the next epoch. A `stable` endpoint's status names its group, and says why it is not known
to be reached where no member holds its address or none passes the probe (Finding 190), the
workload keeping its state and placement. Tested end to end on real nodes, each machine's data
plane in a namespace of the test's own and the client on the edges' link: the client reaches the
server through the edge holding its address, and when that edge loses power, its agent stopped and
its link down, the other edge is granted the address once the holder's term has passed and the
client reaches the server through it. That test found the node behind the edges judging a
connection from an edge's link by the workload's own exposure chain, which knows nothing of edges,
and dropping it; an edge's link is now judged by the edges' chains alone, as the mesh is by its
own. What it does not do: an agent that restarts gives up the addresses it held until its leases
are granted again, a round trip, since it holds nothing it has no lease for. Not built yet: the
probe on a control plane sharing a machine with the node that answers, which this build does not
detect (P3.7e).

Routed edge pools are built 2026-10-26 (P3.7e1). An edge pool naming a router keeps, with each
address's lease, the member its router last said it routes the address to, why its last answer was
not that, and when it is asked again, as a cluster pool's address keeps them (§3.7); the reconciler
that asks a cluster pool's routers asks it too, woken by a lease issued to another member, by one
released, and by a port given back, and asks the router to route the address to the member its
lease was last issued to while that member has not released it and a `stable` port holds the
address, at the member's advertised addresses, and nowhere otherwise. A lease whose term passes
unrenewed moves nothing by itself, since the next member is asked for at its grant. The group's
status shows, for each address, its router, where the router last routed it, and why not to its
holder; and a `stable` endpoint whose address the router has not routed to its holder is shown
unreached with that reason. Tested over the API with a router of the test's own: routed to the
member granted the lease, the router failing when the lease moves and the endpoint shown unreached
for it, and routed to the new holder once it answers. What it does not do: the holder holds the
address from its grant, before the router has routed it, since the lease and not the route keeps
two members from holding it, so the address is unreached for as long as the router takes; and the
router is told one holder, never every member, which is question 14 of §11.

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

**The ingress on one machine, as designed.** Added 2026-10-15, for Phase 1, where the node is its
own edge (§4 of `20-roadmap.md`): the names an endpoint routes, who may route them, what terminates
them, and where certificates come from. An edge tier of Phase 3 takes the same routes and
certificates from the same control plane; nothing here is particular to one machine except that the
proxy runs on the workload's own node.

**A name is routed only by the organization that holds it.** A shared ingress that let any tenant
route any name would let one take another's traffic, and since ACME's HTTP challenge is answered by
whoever the name's traffic reaches, obtain a certificate for it as well. So a name is held: a
`Domain` (§3.6 of `01-model.md`) belongs to one organization and covers itself and every name
beneath it, and an endpoint names only names its own organization holds: the holder of a name is
the organization whose verified domain is the most specific one covering it. A domain is verified
in one of three ways. By DNS: the control plane resolves the TXT record `_korpis-verify.<name>`
through public DNS and finds the token it issued, which shows the holder controls the zone; public
DNS is the system's resolvers, or the one `korpis-server serve --dns-resolver` names where those
would answer from an internal view, and nothing is cached, so a record published a moment ago is
found. By delegation: an organization holding a verified domain creates one beneath it for a
descendant organization, which is how a reseller gives a customer `customer.reseller.example`
without the customer proving anything about a zone they do not run. By attestation: an operator of
the installation vouches for it, for names on a private network that public DNS does not answer and
for an installation's own names. A verified domain of one organization may not cover or lie beneath
another's, except one delegated beneath its parent; a pending one blocks no one. A delegated domain
and its parent both cover the delegated names, which is why holding goes to the most specific: the
reseller that delegated `customer.reseller.example` no longer routes it, though its own
`reseller.example` still covers it (Finding 59 of `23-walkthroughs.md`). Deleting a domain deletes
the domains delegated beneath it, whose verification rested on it. **What evaluates this, where,
and with what information:** the control plane, at verification, from its own records of every
verified domain and from the DNS answer it obtains itself; and again at every declaration, from the
same records. What it does not do: check again later. A zone that changes hands keeps its domain
verified here until its holder deletes it; what limits the damage is that every certificate
issuance proves control of the name anew (below), so the new owner of the zone, pointing its
records elsewhere, takes the traffic and no certificate can be issued for the old holder's
endpoint. Whether to re-verify periodically is question 10 of §11.

**A name routes to one endpoint, on the node its workload runs on.** An endpoint whose exposure is
`ingress` lists its names, one to a hundred (what one ACME certificate holds), each a plain
lowercase DNS name with no wildcard (question 11 of §11) and no last label of digits alone, which
would make it an IPv4 address, never a name a client asks TLS for (Finding 60 of
`23-walkthroughs.md`); its protocol is `http` or `https`, with `tls` `acme` (the default) or
`provided`, or `tcp` for a TLS service routed by SNI without termination (`tls.source:
passthrough`); and its container port is where its workload listens. An endpoint of any other
exposure names no domain and declares no TLS: it is reached at its address, and a name for that
address is a record, not a route. Two endpoints may not route one name in the whole installation,
which the control plane keeps by holding the names it routes in one place, keyed by name: it checks
them at declaration, and claims them when a plan moves the workload's target, under the lock
verification takes, checking again that the workload's organization holds each; a target that no
longer names one, and the workload's retirement, free it. Because a name stays routed only while
its organization holds it, a domain cannot be deleted, nor delegated to another organization, while
that would leave a name an endpoint routes without its holder; the change is refused, naming the
name, until the endpoint stops routing it. The control plane places the workload on a node that can
serve the ingress, which a node measures at start and advertises (it holds ports 80 and 443 of its
addresses), and the node's assignment carries the endpoint's names, its TLS source, and its
certificate's identifier. The endpoint's address is the node's advertised addresses on port 443,
and on 80 for the redirect and ACME's HTTP challenge; publishing the names in DNS is the holder's,
or the DNS provider's of its zone, which publishes `A` and `AAAA` records for the node's addresses
as it publishes any endpoint's (§3.5). A workload that moves to another node takes its routes with
it, and its names then need their records pointed at the new node, which a DNS provider does and a
holder publishing by hand must; that is what `ingress` survives on one machine, and an edge tier
makes it survive without the records changing.

**The proxy is a process of its own on the node.** It is the node's most exposed surface: it parses
requests from anyone on the internet. So it is not the agent: `korpis-ingress` is a separate
program the agent supervises, confined like a backup job (§5.5 of `06-storage.md`): no filesystem,
no capability, no port of its own to bind, and its only network the sockets it is given and new
connections out through the node's workload links. The helper opens ports 80 and 443 on the node's
advertised addresses, which needs a privilege neither the agent nor the proxy has, and passes the
sockets to the agent, which hands them to the proxy, as the resolver's are handed (§5.1). The agent
has the helper start the proxy, as the agent's own user and in a cgroup of its own, because only
the helper can make a cgroup and the cgroup is what confines where the proxy connects: the node's
table drops a new connection from it that leaves by anything but a workload's link, which Landlock,
naming ports and not addresses, could not express. Where the node cannot make the cgroup or the
rule, the agent says the proxy runs without that part of its confinement. When the proxy goes away
the agent has it started again, waiting longer each time it fails quickly; the node says it serves
the ingress while the agent holds the sockets, and says so as soon as it holds them rather than at
its next heartbeat, so placement does not wait on it; an operator who wants a node to serve none
runs its agent with `--ingress false`. The agent gives the proxy its routes and certificates whole,
again whenever they change, over a Unix socket it names when it starts the proxy, and hands the
listening sockets over the same connection as `SCM_RIGHTS`, so the proxy adopts them without
inheriting descriptors by number; the proxy keeps nothing on disk. (Corrected 2026-10-15, when the
proxy was built: this said a pipe, which carries no descriptors.) It accepts HTTP/1.1 and HTTP/2
from clients over TLS 1.2 and 1.3, chooses the certificate by SNI, redirects plain HTTP to HTTPS
except for ACME's challenge path, routes by the `Host` header among the names the connection's
certificate serves, and forwards to the workload's address over HTTP/1.1, with WebSocket upgrades
carried through; for an endpoint whose protocol is `https`, within TLS to a workload serving its
own certificate, which the proxy does not verify, since the workload made it for itself and no
authority the proxy holds vouches for it, and checks only that the workload holds its key. That TLS
hop costs a handshake and protects nothing the node could not already see, since it never leaves
the node; it exists for workloads that serve only HTTPS. It removes any `Forwarded` and
`X-Forwarded-*` header a client sent and sets its own, so the workload learns the client's address
from a header only the proxy writes; the endpoint's `client_address` is `translated`, since the
workload's peer is the proxy (§3.4). A `passthrough` endpoint is routed by the SNI of the client's
first message and spliced to the workload unread, which then terminates TLS itself and sees the
proxy as its peer. It bounds what it accepts: header size and count, the time a client may take to
send them, the time a connection may stay idle, and connections in all, where a WebSocket opened
on a stream of an HTTP/2 connection counts as a connection of its own while it is open, since it
holds a connection to its workload, and an HTTP/2 connection carries at most 100 streams at once
(corrected 2026-10-22, Finding 138 of `23-walkthroughs.md`: such a WebSocket was not counted, so
one connection could hold as many to its workload as it opened streams). A head too large or with
too many fields is refused 431; a connection beyond the bound is answered 503 on port 80 and closed
on 443, where there is no TLS yet to answer in, and a WebSocket beyond it is answered 503 on its
stream; a head not sent in time, a TLS greeting not sent in
time, and a connection idle past its bound are closed, as HTTP lets a server do with a connection
it gives up on. A request naming a name its connection's route does not serve is refused 421,
and a client asking for a name nobody routes, or one with no certificate yet, gets no handshake.
What it costs: one more process per node, and a hop through it for every request; and a request's
body is streamed, not buffered, so a slow workload holds the client's connection open as long as
the workload does.

> Corrected 2026-10-22 (Finding 137 of `23-walkthroughs.md`). This said a request naming a name
> its connection's certificate does not serve is refused, which a wildcard certificate shared by
> two endpoints makes every name of both; the proxy as built refuses any name outside the route its
> connection's name chose, which is the rule, and what keeps a certificate one route asks for from
> being skipped by a connection made for another.

**Certificates belong to the control plane.** An endpoint's TLS is `acme` unless it says otherwise:
the control plane obtains one certificate for the endpoint's names from the ACME directory the
operator configures (Let's Encrypt by default), with the installation's ACME account, and renews it
when two thirds of its life have passed. The account is made at the first order, and only if the
operator agreed to the directory's terms; without that agreement no `acme` certificate is issued
and each says why (§7 of `18-operations.md`). Every order makes a key of its own for its
certificate, which leaves the control plane only sealed and only to the node serving it. It answers
ACME's challenge for each name on its own, by DNS where a DNS provider of the holder's organization
serves the name's zone, publishing the `_acme-challenge` TXT record through it (§3.5), and
otherwise over HTTP. The holder is the endpoint's organization, which alone routes the name, and
its DNS providers are those it sees, its own before its ancestors', as a call is routed (§3.3 of
`16-extensions.md`): each is asked in turn to publish the record until one does, a provider that
manages no zone holding the name answering `dns.zone_not_found`. **What evaluates whether a
provider serves the zone, and with what information:** the provider, from the zones it manages; the
control plane holds no list of zones (§3.5), so it learns by asking, at the cost of one refused
call per provider ahead of the one that serves. The record is applied for an owner of its own, the
certificate and the name's place among its names (`crt_…/0`), so it never disturbs the endpoint's
records, with a time to live of a minute. It is recorded before the provider is asked and withdrawn
when the order ends, however it ends, so one an issuer published before it stopped is withdrawn by
the next issuer that looks; a certificate is not removed while one of its records stands. A
provider answers once its zone is changed, which its authoritative servers may not yet serve, so
the control plane tells the directory to look once its own resolver finds the record (the system's,
or the one `--dns-resolver` names, as for verification), or after two minutes if it never does. A
provider that serves the zone but cannot be reached leaves the name to HTTP, which may still
succeed. Over HTTP: the control plane gives the token to the node the endpoint's workload is placed
on, whose proxy answers it on port 80, which works once the name's records point at that node. The
token travels in the workload's assignment, and the proxy takes it from the set its node holds
rather than from the agent's reconciliation, so a long pass does not hold it back; the control
plane tells the directory to look only once every node serving the names has acknowledged a set
carrying it, and two seconds more. An `acme` certificate is its endpoint's, made when the
endpoint's routes are claimed: an endpoint whose names change is given another, and one nothing
names is removed. A failed attempt is retried after five minutes, doubling to a day, which keeps an
installation inside a directory's limits on failed validations; a certificate issued before keeps
serving meanwhile; a restart of the control plane tries every certificate not yet issued at once. A
`provided` certificate is one its organization uploads, chain and key, and the endpoint names it by
its identifier in `TlsSpec.secret`. The control plane checks at upload that the key is the leaf's,
by their public keys, and that the leaf names DNS names and has not expired; and, wherever an
endpoint names it, that it is the endpoint's organization's, valid now, and covers every name the
endpoint routes, a wildcard covering one label, at declaration and again when the routes are
claimed, the certificate locked against change until the claim commits. Its organization replaces
it in place, keeping its identifier, with a chain that still covers every name served with it,
which is how a provided certificate is renewed without declaring the endpoints again; it cannot be
deleted while an endpoint serves with it, which the route records. The control plane says when one
nears expiry, but it cannot renew it: every issued certificate, of either source, is a
`certificate.expiring` event in its organization once five sixths of its life have passed, a
warning, and critical at seventeen eighteenths, each once for one validity, the control plane
looking hourly; measured in its own life, so a certificate of days and one of a year are each told
in time, and an `acme` one, renewed at two thirds, is told only when its renewal is late. Every
private key is stored sealed by the control plane's TLS key, in its data directory beside its
backup key (§6 of `18-operations.md`), and a node obtains an endpoint's certificate and key by
asking for it over its session (`CertificateRequest`, §4.2 of `02-architecture.md`), which is
answered only to a node an endpoint served with it is placed on. The endpoint as placed names the
certificate and its revision, which every replacement or renewal raises, so the node's assignment
changes and the node asks again. The node keeps what it serves in its state directory, readable by
the agent alone, so a restart while the control plane is unreachable still serves (§4.6 of
`02-architecture.md`), and removes a certificate, key and all, once no endpoint placed on it serves
with it. **What evaluates this, where, and with what information:** issuance and renewal, the
control plane, which holds the endpoint, its placement, its holder's DNS providers, and the
account; the challenge's answer, the proxy, from the token it was given; which certificate a
connection gets, the proxy, from the SNI the client sent. What it costs: a node holds the private
keys of the endpoints placed on it, which a compromised node could take, as it could already read
the workloads' own memory, and which removing a node as compromised therefore revokes and replaces
(§9 of `17-security.md`); and an issuance that fails, because the name does not point at the node
yet or the directory refuses, leaves the endpoint served on 80 alone, its status saying why, and is
retried after the back-off. The status shows each ingress endpoint's certificate beside the
workload's endpoints: its state, its validity, and for one not issued its reason, which are the
control plane's own records rather than an observation a node reported. (Corrected 2026-10-16, when
it was built: this said the endpoint's observation, which is what a node reports, and no node knows
why a directory refused.)

What the ingress does not do in Phase 1, stated plainly: HTTP/3, which needs the UDP path an edge
tier brings; HTTP/2 or gRPC to the workload, since a backend's protocol beyond HTTP/1.1 is not yet
something an endpoint declares (question 12 of §11); WebSocket over HTTP/2; ACME's TLS-ALPN
challenge, which the HTTP and DNS challenges make unnecessary where port 80 or a DNS provider is
there; client certificates; and the PROXY protocol for a `passthrough` workload that wants its
client's address. HTTP/3 arrives with edges in Phase 3, and the rest of these in Phase 2, except
the backend's protocol, which is a question (§4 of `20-roadmap.md`). (Corrected 2026-10-22, when
three of them were built, below, and proven on a real node: WebSocket over HTTP/2, client
certificates, and the PROXY protocol. This paragraph also placed the TLS-ALPN challenge at a
node's ingress in Phase 2, which §4 of `20-roadmap.md` did not list; it was listed there, and
built the same day, below. What the ingress still does not do is HTTP/3 and the backend's
protocol.)

**WebSocket over HTTP/2, client certificates, and the PROXY protocol, designed 2026-10-22** for
Phase 2, and built the same day. Each is decided by what evaluates it, where, and with what: all
three are evaluated by the node's proxy, from the route its agent gives it, which the agent builds
from the endpoint as its assignment declares it, so no step asks the control plane anything at
connection time.

*WebSocket over HTTP/2.* The proxy announces extended CONNECT on HTTP/2 (RFC 8441), so a browser
whose connection to a name is HTTP/2 opens a WebSocket on it, as a stream of that connection,
rather than on a second connection of HTTP/1.1. A `CONNECT` with `:protocol websocket` is
forwarded to the workload as the HTTP/1.1 upgrade the workload already understands, with the
request's path and headers, and once the workload answers `101` the client is answered `200`,
which is how HTTP/2, having no `101`, says it, and the stream and the upgraded connection are
joined both ways; any other answer is returned as the stream's response (`research/evidence.md`
§5). Nothing is
declared: the workload sees what it sees from an HTTP/1.1 client, with a `Sec-WebSocket-Key` the
proxy makes, since an HTTP/2 client sends none, and the client is given the subprotocol and
extensions the workload chose (built 2026-10-22). What it costs: a WebSocket's
frames share their client's connection with every other request on it, so one slow to read holds
back flow control for that stream alone, as HTTP/2 does for any stream.

*Client certificates.* An endpoint whose TLS the proxy terminates may ask clients for a
certificate: `TlsSpec.client_certificates` is `required` or `optional`, and `TlsSpec.client_ca`
holds, in PEM, the certificates of the authorities it trusts, which are public and travel in the
declaration, checked at declaration to be certificates, at most sixteen of them and 64 KiB in all.
The proxy chooses a TLS configuration by the name the client asks for, which it reads before the
handshake as it does for `passthrough`, so the name's authorities, and no other route's, are the
ones a certificate is verified against. A `required` endpoint completes no handshake without a
certificate those authorities issued; an `optional` one completes it and tells the workload none
was given. The proxy passes the verified certificate to the workload as RFC 9440 says, in
`Client-Cert`, and the chain the client sent in `Client-Cert-Chain`, and removes both from every
request a client sends, to every route, so a workload reads them only as the proxy wrote them. A
`passthrough` endpoint asks for nothing: its workload terminates TLS and asks for certificates
itself. As built (2026-10-22), the declaration's PEM is read as the proxy reads it: each section
must be a certificate that serves as a trust anchor, and any other section, a private key pasted
among them above all, is refused rather than skipped and published. A route that asks for
certificates is accepted with a TLS configuration of its own, whose sessions and tickets are its
own, so a session made where no certificate was asked for is never resumed where one is; the
cost is that its sessions are not resumed across a change to the node's ingress configuration,
which makes the configurations anew. The certificate a client presented reaches the workload as
a Byte Sequence of its DER in `Client-Cert`, and the rest of the chain, if it sent one, as a List
in `Client-Cert-Chain`. What it does not do: it checks no revocation, neither a CRL nor OCSP,
so a certificate an authority revoked is accepted until it expires, and an endpoint that must
refuse one sooner issues short-lived certificates or checks itself, which the header lets it; and
a client refused at the handshake sees its browser's error, not a page, because there is no TLS
session to answer in. A request is served only by the route the connection's own name chose,
whose configuration the handshake was made in, and any other is refused 421: a browser reuses an
HTTP/2 connection for every name its certificate covers, and otherwise a name that requires a
certificate would be reached on a connection made for one that does not (Finding 137 of
`23-walkthroughs.md`).

*The PROXY protocol.* A `passthrough` endpoint's workload sees the proxy as its peer (§3.4). Its
endpoint may declare `client_address: proxy_protocol`, and the proxy then writes a PROXY header,
version 2, naming the client's address and port and the address it was reached at, before the
first byte it splices; the endpoint's observed `client_address` says `proxy_protocol`. It is
refused at declaration on any other endpoint: one the proxy terminates tells its workload by
`Forwarded` already, and one no proxy carries has no proxy to write the header. The refusal is
held again where the endpoint is stored and where the proxy reads its route, so no later step
carries a header to a connection the proxy terminates (built 2026-10-22). The header is TCP over
IPv4 when both addresses are IPv4, an IPv4 address mapped into IPv6 by a dual-stack socket
counting as one, and TCP over IPv6 otherwise; it carries no TLVs, since the proxy has read
nothing of the connection but its SNI, which the workload reads itself. The address reached is
the one the node's socket was reached at: behind a port forward or a NAT that is the node's own
address, not the one the client dialled, which no node can know. What it costs: a
workload that is told to expect the header and does not parse it reads the header as its
client's first bytes and fails every connection, which is the declaration's to get right; and a
workload that parses it must accept it only from the proxy, which it can, since only the proxy
reaches a `passthrough` workload's port through the ingress.

**ACME's TLS-ALPN challenge at a node's ingress, designed 2026-10-22** for Phase 2, and built
the same day. A name of an
`acme` endpoint that no DNS provider proves is proven by the node's proxy either way a directory
reaches a node: over HTTP-01 on port 80, as above, or by TLS-ALPN-01 (RFC 8737) on port 443. Which
is decided by the issuer, from the certificate's own record: the way that last proved it, kept
with the certificate when it is issued, and HTTP-01 for one never issued; and after an attempt
fails, the next attempt is made the other way, so the two alternate until one succeeds, and the
one that did is used first at every renewal. So a node whose port 80 a provider or a firewall
blocks has its certificate on the second attempt and keeps it, without anyone declaring that its
port 80 is closed, which no component could check. The digest travels in the workload's
assignment beside the HTTP-01 tokens, with the name it proves, and the issuer tells the directory
to look once every node serving the names has acknowledged it, as for HTTP-01. The proxy answers a
`ClientHello` that offers `acme-tls/1` as its only protocol, for a name it holds a digest for,
with a certificate it makes for that name from the digest, critical `acmeIdentifier` extension and
all, speaking `acme-tls/1` alone, and closes the connection after the handshake; one offering it
for a name it holds no digest for is routed as any other, so a `passthrough` workload that runs
its own ACME client is still validated by its own answer. **What evaluates this, where, and with
what information:** which way, the issuer, from the certificate's attempts and its record of the
way that last proved it; the answer, the proxy, from the name and digest its node holds. What it
costs: a first issuance on a node whose port 80 is closed fails once, a failed validation the
directory counts and five minutes of back-off, before it succeeds; and a node reached on neither
port at the name's address is proven neither way, and stays `failed` with the directory's reason,
as before.

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

> Corrected 2026-10-26 (Finding 188 of `23-walkthroughs.md`), when edges were designed: the
> paragraph below had the inbound packet encapsulated in WireGuard between Korpis machines, and
> WireGuard accepts a decrypted packet only from a source within the sending peer's allowed IPs
> (§9.1), so a packet keeping a player's address is dropped by the node it is sent to. The edge
> carries it in GRE inside the mesh instead, the outer header from the edge's own address (§3.1).

**Edges and forwarding nodes tunnel; they do not translate the source.** The inbound packet is
encapsulated (WireGuard between Korpis machines, which the overlay already provides, §9) and
delivered to the workload's network namespace with its original source address. The reply follows
policy routing back into the tunnel to the edge, which sends it from the stable address (as
designed 2026-10-26 in §3.1: GRE inside the mesh, so the outer header is the edge's own). The
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
object the records exist for, an endpoint (`ep_…`), or one name's ACME challenge of a certificate
(`crt_…/<n>`, §3.2), and the call `ApplyRecords` carries every record that owner should have: `A`,
`AAAA`, `CNAME`, `SRV`, or `TXT`, each a lowercase name, a TTL of 30 to 86 400 seconds, and its
values, at most a hundred records per owner. The provider creates what is missing, changes what
differs, and removes what the owner had and no longer has; an empty list removes them all. It never
touches a record it does not own, so it must know which are Korpis's and whose (a TXT ownership
marker beside each, the way external-dns keeps its registry, or the provider's own tags). A name in
no zone it manages fails `dns.zone_not_found`, and a name already held by a record it does not own
fails `dns.conflict`, neither retryable; an unreachable DNS service fails retryably. Every record
is checked by the control plane before any provider is called, so all providers are asked the same
well-formed thing.

The reason for whole sets rather than add and delete: the call is then idempotent, so a retry after
a lost answer, a provider replaced mid-way, or a resync after an outage all converge on the same
records, and the control plane's reconciliation, which already works from desired state, needs
nothing else. What it costs: every change resends the owner's whole set, a hundred records at most,
and every provider must keep an ownership registry, which is the price of never deleting a record a
person made. Which names an organization may publish is decided before any provider is asked: only
names an ingress endpoint routes are published, and a name is routed only by the organization
holding it (§3.2). (Corrected 2026-10-16, when endpoints' records were built: this said zones were
not objects, so a provider published whatever its organization's endpoints asked for until zones
became objects. Zones still are not, but domains are, and holding a domain is the check.) **As
built for endpoints.** Each name an ingress endpoint routes resolves to the addresses its node
advertises, an `A` set and an `AAAA` set with a time to live of a minute, so a workload that moves
is found at its new node a minute after its records change. Each name is applied for an owner of
its own, the endpoint and a digest of the name (`ep_…/…`), so each is published through whichever
provider serves its zone, asked in the order of §3.2: the one that published it before, then the
others the organization sees, its own before its ancestors'. A name no provider serves is the
holder's to publish, and the workload's status says so beside the name, `unmanaged`; the providers
are asked again hourly, so one installed meanwhile takes it over. A name no longer routed is
withdrawn from the provider that published it. A change of an endpoint or of its node's addresses
wakes the reconciler, which also looks every thirty seconds, and takes each name under a lease, so
replicas share the work. A failure does not touch the workload, which runs: the name is `failed` in
the workload's status with the provider and its reason, and is retried after five minutes, doubling
to a day. (Corrected 2026-10-16: this said the endpoint's observation became `unsatisfiable`, but
an observation is what a node reports, and no node knows what a DNS provider answered.) What it
does not do: publish anything for an endpoint of any other exposure, which names no domain in Phase
1.

### 3.6 The control plane under an organization's name

> Designed 2026-10-21, for Phase 2 (§4 of `20-roadmap.md`), resolving Finding 66 of
> `23-walkthroughs.md`: a reseller's customers were to reach the panel, sign in, and hold passkeys
> at the reseller's own address, and nothing served the control plane under any name but the
> installation's own.

**A portal is a name an organization holds, under which the control plane serves itself.** A
`Portal` (§3.6 of `01-model.md`) belongs to one organization and names one DNS name, which the
organization must hold as an endpoint's names are held (§3.2): the holder of the name is the
organization whose verified domain is the most specific one covering it. At a portal the control
plane answers what it answers at its own address, the API, the panel from `--web-root`, and
`/about/source`, on the port of `--public-url`, so a portal's origin is `https://<name>` for an
installation reached on port 443. What differs is whom it is for: the panel wears the theme of the
portal's organization (§7 of `11-surface-web.md`), the sign-in page offers that organization's
identity providers, a passkey registered there is bound to the portal's name, and a session begun
there acts only beneath that organization (§9.8 of `08-identity.md`). A name is a portal or an
endpoint's route, never both, and two portals never share one: the control plane keeps the
portals' names beside the names endpoints route, under the lock verification takes, and refuses a
portal for a name an endpoint routes and a route for a portal's name. A portal is never the
installation's own host, nor the host of a `--web-origin`, which the control plane serves already.
A domain cannot be deleted, nor delegated to another organization, while a portal's name needs it,
as for a route. Making and deleting a portal is `portal.write` on its organization, and reading
one `portal.read` (§5 of `10-api.md`); an organization makes as many as it holds names for
(Principle P10). The name's records are the holder's to publish, a `CNAME` to the installation's
host or its addresses: the control plane does not publish them, because it does not know the
addresses it is reached at, `--public-url` naming a host that may resolve to a load balancer.

**A portal's certificate is its own, and the control plane serves it.** A portal's TLS is `acme`
unless it names a `provided` certificate of its organization covering its name, checked as an
endpoint's is (§3.2). An `acme` certificate is the portal's, made with the portal, for its one
name, and issued and renewed by the same issuer, from the same directory and account, with the same
backoff and the same expiry warnings as an endpoint's. Only the proof differs: no node serves a
portal, so HTTP-01 has nowhere to be answered. The issuer proves the name by DNS where a DNS
provider of the organization serves its zone, asked in the order of §3.2, and otherwise by
TLS-ALPN-01 (RFC 8737), which the control plane answers on its own listener. The issuer records the
challenge's digest for the name before it tells the directory to look, and withdraws it when the
order ends, however it ends; a connection whose client offers only the protocol `acme-tls/1`, for a
portal's name, is answered by whichever replica it reaches with a certificate made for the
challenge from the digest, read from the database for that handshake, and is then closed. So it
works on any replica, behind a load balancer that passes TCP, without the replicas agreeing on
anything first. What it needs: the name's records pointing at the control plane, and the control
plane reached on port 443 at them, the only port a directory validates TLS-ALPN-01 on. A name the
control plane is not reached at on 443, and no DNS provider serves, is given a `provided`
certificate instead, or stays `failed` with the directory's reason.

**Which certificate a connection is given, and which portal a request is for.** Each replica holds
the portals and their certificates in memory, read again when the database announces a change to
either and every minute in case an announcement was missed, and chooses by the name the client
sends in SNI: a portal's name is served with its portal's certificate, and any other name, or none,
with the installation's own (§7 of `18-operations.md`), as before. A portal whose certificate is
not issued yet gets no handshake, as the proxy answers a name with no certificate (§3.2), and the
portal says why, `pending` or `failed` with the reason. A request on a portal's connection whose
`Host` names another name is refused 421, so the portal a connection proved is the portal its
requests are for (Finding 127 of `23-walkthroughs.md`); and since a portal's certificate names its
one name, a browser never carries another name's requests on it. The panel at a portal opens
console and file sessions at relays as it does at home: the control plane names the portals'
origins to every relay in the relay's feed, and a relay that is its own process answers them beside
its `--web-origin` (§3.1 of `02-architecture.md`, Finding 132 of `23-walkthroughs.md`).

**What evaluates this, where, and with what information:** whether a name may be a portal, the
control plane, at creation, from its records of verified domains, routes, and portals, under the
lock verification takes; the certificate's issuance, the issuer, as for an endpoint, its proof
answered by whichever replica the directory reaches, from the database; the certificate a
connection is given, the replica's listener, from the SNI and the portals it holds; the portal a
request is for, the listener again, from the certificate it served; and the relying party, the
theme, the providers, and the reach of a session, the control plane, from that portal (§9.8 of
`08-identity.md`).

What it costs, and what it does not do. A portal needs the control plane's own listener to
terminate its TLS: behind a load balancer that terminates TLS itself, the control plane sees
neither the name nor the handshake, and a portal's key never leaves it but sealed, so an
installation serving portals has its load balancer pass TCP, with the PROXY header of §3.1 of
`02-architecture.md` for the client's address. Every portal's traffic is the control plane's own
load, on its own listener. And a portal hides nothing about the installation: it is the same
control plane under another name, and a customer who looks finds the same API, the same server
information, and the same source page at it as at home.

> Built in part, 2026-10-21 (P2.9b of the build plan): the portal itself. `PortalService` makes,
> reads, re-certifies, and deletes portals under `portal.write` and `portal.read`, with
> `certificate.read` on a provided certificate it names, and `korpis portal` calls it (§5 of
> `13-surface-cli.md`). A portal is checked under the domains lock against the domains, the
> routes, and the other portals (migration 0110), and the installation's own hosts are the
> origins its relying party is configured with. An endpoint naming a portal's name is refused
> `endpoint.name_portal`, at declaration and at its claim; a delegation or deletion that would
> take a portal's name from its organization is refused `domain.portal`; a provided certificate
> a portal is served with is neither deleted nor replaced by a chain that no longer covers its
> name. A portal's own `acme` certificate is made with it and recorded with the portal it is for;
> once no portal names it, the issuer's sweep removes it as it removes an endpoint's. Until the
> listener answers TLS-ALPN-01 (P2.9c), such a certificate is issued only where a DNS provider of
> the organization serves the name's zone, and otherwise stays `failed` with that reason.
> (Answered since P2.9c, below.)
>
> Built 2026-10-21 (P2.9c): serving. The listener reads each connection's ClientHello before it
> chooses a configuration: a portal's name is served with its portal's certificate and its
> requests told which portal they are for; the name of a portal whose certificate is not issued,
> or not valid now, gets no handshake; any other name, or none, is served with the installation's
> own certificate. Each replica holds the portals in memory and reads them again on
> `korpis_portal`, which a portal's change and a raised revision of its certificate announce
> (migration 0111), and every minute. A ClientHello offering only `acme-tls/1` for a name is
> answered with a certificate made for the order's challenge, the digest read from
> `portal_challenge` for that handshake, and nothing is served after it; the issuer records the
> digest before telling the directory to look and withdraws it when the order ends. At a portal,
> a request naming another host is refused 421 and a cross-origin call is answered for the
> portal's own origin alone; a relay's session (`korpis.v1.RelayService`) is answered for every
> portal's origin at the installation's own address too, and a relay that is its own process is
> sent the portals' origins on its feed (`PortalOrigins`) and answers them for its sessions. The
> API test proves a portal by TLS-ALPN-01 through Pebble against the control plane's own
> listener, verifies the served chain for the portal's name against Pebble's issuing root, and
> finds the 421 and the cross-origin rule.

### 3.7 `dedicated_ip`: an address of the workload's own

Designed 2026-10-26, for Phase 3 (§4 of `20-roadmap.md`, trace 77 of `23-walkthroughs.md`). A
workload with any endpoint exposed `dedicated_ip` holds one IPv4 address of its own, from an
operator's address pool (§4), and every such endpoint of it is reached at that address on its own
port; what it sends beyond its node leaves from that address too, which is what IP reputation
means. IPv6 has no dedicated mode: every workload's IPv6 address is its own already (§4), so a
`dedicated_ip` endpoint is reached over IPv6 as any endpoint exposed at the workload's address is.

*Whose pool.* An address pool belongs to an organization. An operator makes it for that
organization with `address_pool.attest` on the installation's root organization, vouching that the
organization holds the range, as `domain.attest` vouches for a name (§3.2), and the organization
deletes it with `address_pool.write`; a host's is the root's. It serves the workloads of that
organization and of those below it, and its addresses are held on that organization's own nodes
alone: a pool of scope `node` names one of them, the one its addresses are routed to, and a pool of
scope `cluster` names the router that moves an address to whichever of them holds it. Not on a node
of an organization above: a node that holds an address translates every connection to it, and
answers for it on its link where the pool says so, so a pool on another's node would let its owner
take that node's traffic to any address it named, a public resolver's or the node's own gateway,
into its own workload. Korpis gates none of this by size or count (P10): a host gives itself as
many pools and addresses as its networks hold.

> Corrected 2026-10-26 (Finding 187 of `23-walkthroughs.md`, from an automated security review of
> the commit that built pools): the organization made its pools itself, with `address_pool.write`.
> Nothing proves who holds an address range, and no two pools overlap, so a tenant could take a
> range another organization holds and leave it unable to make its pool, a squat the owner could
> only ask an operator to clear. Making a pool is now the operator's attestation. Not a limit on
> its size: an operator vouching for a range is the check, and P10 rules out the other.

*Which address, and when.* The control plane allocates the address in the transaction that places
the workload, the lowest free one of a pool that serves it and reaches the chosen node, and records
it against the workload; every `dedicated_ip` endpoint of the workload's shows it. The placement
filter takes a node only where such a pool has a free address, or where the address the workload
holds already reaches it (§2.1 of `05-scheduling.md`). Admission counts one `dedicated_ipv4` for a
workload with any `dedicated_ip` endpoint, however many it has, since they share the address
(Finding 185 of `23-walkthroughs.md`). When the workload is retired the address goes back to its
pool after the pool's quarantine, a day unless the pool says otherwise, so that what still points
at it, a client's cache or a name not yet changed, does not reach the next holder at once.

*How the node holds it.* The assignment carries the address. The helper translates every
connection to it into the workload's own address, all ports alike, ahead of everything else on the
node, so the workload's namespace still accepts only its declared ports (§4) and the client's own
address is kept (§3.4); and what the workload sends beyond the node, by any interface but another
workload's link and the mesh's, leaves translated to the address rather than the node's. A pool
whose addresses are routed to the node, which is how hosting providers deliver an additional
subnet, needs nothing more; one whose addresses are on the node's link, answered by address
resolution, names the node's interface, and the helper adds the address there, as a host address
without a route, and removes it when the workload leaves. A connection to the address from outside
the node is one the node translated, so the rule of §4 lets it in.

*Where it may run.* An address of a `node` pool is routed to that node alone, so a workload holding
one is placed there and nowhere else, and a migration of it is refused, saying why; this is
`routing: static`. An address of a `cluster` pool follows the workload: the pool names an address
router, an extension providing the `address_router` kind (§5 of `16-extensions.md`), which the
control plane tells, for each address, the node that holds it and that node's advertised address,
and which tells it back once the network routes it there; the node holds the address only once the
router has said so, so two nodes never hold one address at once. The router does what the network
needs: announces the address over BGP with the node as its next hop, or moves a provider's floating
address through the provider's API (`routing: bgp | provider_api`). A cluster pool naming no router
its organization sees is refused when it is created. At a migration's cutover the router is told
the new node after the lease's epoch has changed (§10).

*The router's contract.* Designed 2026-10-26 (P3.6d in §4 of `20-roadmap.md`). A pool names its
router as a backup repository names its target (§5.6 of `06-storage.md`): by the name of an
`address_router` provider its organization sees, its own before its ancestors'. One call,
`RouteAddress`, carries an address, the node that is to hold it or none, and that node's advertised
addresses as the next hops, and asks the router to make the network route the address so, as
`ApplyRecords` asks a DNS provider to make an owner's records so: the whole of what is wanted in
every call, so a retry or a repeat changes nothing. The control plane keeps, for each address of a
cluster pool, the node the router last said it routes the address to. A loop like the one
publishing names takes each address whose holder is placed on another node, or that rests and is
still routed, holds it for a while so that one control plane asks, and asks the router; a failure
is retried on a backoff and shown on the workload, with the router named and its reason. A node's
assignment carries the address only once the router has answered for that node, so what evaluates
"two nodes never hold one address" is the control plane, which writes the routed node only on the
router's answer and gives the address to that node's assignment alone, and the router is asked only
for the node of a placement already committed, whose old node's assignment has already lost the
workload. What it does not do: the node the address left answers for it until it applies its new
set, one assignment round, which is what any move of an address costs; and the router's own view of
the network is the router's, so an address it says it routes and does not is the router's defect,
shown only as the workload unreached.

What it costs: an address idle for its quarantine after every retirement; a router extension, and
the network or provider that does what it asks, for an address that moves; and a workload of a
`node` pool that cannot move off its node. What it does not do: it gives a workload one address
and no more; it does not announce anything itself, since BGP sessions and provider credentials are
the router's; and it does not make the host's own processes use the address, which only the
workload sends from.

Built 2026-10-26 for `node` pools, with their addresses routed to the node or on its link. The
control plane keeps the pools and their addresses (`korpis address-pool`, §2 of
`13-surface-cli.md`), each address with the workload holding it or the instant it was released; the
placement filter's `dedicated_ip` check and the allocation both run under the placement lock, the
allocation in the transaction that binds the workload; and a retirement gives the address back
under that lock. The node's helper keeps two maps in the host's table: from each address to its
workload's chain, translating every new connection to it in chains of priority -110, ahead of the
node's forwarded ports (-100) and of any other table's translation of a destination, and from each
workload's link to its address, translating what it sends beyond the node in the host's
`postrouting` chain ahead of its masquerade. An address on the node's link is added to the pool's
interface as a host address, `/32`, with no route of its prefix; the workload's chain records in a
rule's comment that the helper added it, so an address the operator had already put on the
interface is never removed. The router's contract followed the same day (P3.6d): `cluster` pools
are made naming an `address_router` provider their organization sees, `RouteAddress` is in the
contract and the SDK (`provider.address_router`, §6 of `16-extensions.md`), the control plane keeps
each address's routed node with a reconciler like the one publishing names, retried ten seconds
after a failure and doubling to ten minutes, a node's assignment carries a cluster pool's address
only for the node recorded, and `korpis status` shows an address waiting on its router with the
router's reason; a router of the tests' own exercises it. Edge pools are made since 2026-10-26,
naming the edge group that holds them (§3.1). Not built yet: moving a workload holding an address,
since no workload migrates yet (§8 of `05-scheduling.md`): a cluster pool's address follows its
workload to whichever node it is placed on next, and one holding a node pool's is placed on no
other node.

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

> Corrected 2026-10-26 (Finding 180 of `23-walkthroughs.md`). The firewall in the workload's
> namespace accepted a declared port from whoever sent to it, and nothing on the host asked where
> a connection came from, so "where declared" meant any declared port from anywhere a packet
> could be routed: on a node with a routed IPv6 prefix, the internet. An `overlay_only` endpoint
> was reached past its node by anyone, though §3 says other workloads alone reach it, and an
> `ingress` endpoint directly, past the proxy's TLS, client certificates, and limits (§3.2). A
> new connection forwarded to a workload's link from outside the node, from neither another
> workload's link nor the mesh, is now let in only where the node translated it from a port of
> its own, or to a port of an endpoint exposed at the workload's own address: `node_port`,
> `node_bound`, and `dedicated_ip`. The rest is dropped and counted under the comment
> `exposure`. The helper holds it in a chain of the workload's own in the host's table, reached
> through a map keyed by its link, as its egress is (§6). What it does not do: the node's own
> processes reach every declared port, which is how the ingress proxy reaches its workloads, and
> what the mesh carries is judged by the mesh's own chains (§9.1). What it costs: an `ingress` or
> `stable` endpoint is no longer reached at the workload's own IPv6 address, which nothing said
> it was; one that should be is declared `node_port`.

This inverts what every panel in this market does, and it is worth doing because IPv4 is the single
largest per-workload cost a hosting operator carries. An operator who can serve IPv6-capable
clients natively and reserve IPv4 for those who need it has a materially cheaper cost structure.
Pelican added IPv6 allocations; treating IPv6 as the default rather than an option is the next
step.

An `AddressPool` (§3.6 of `01-model.md`) is a prefix an organization's network routes to its
nodes, of `kind` IPv4 or IPv6, `scope` node, edge, or cluster, and `routing` static, BGP, or a
provider's API. `routing` determines whether a `dedicated_ip` can move between nodes. Static
routing means the IP belongs to one node and a workload holding it cannot migrate. BGP or a
provider API means the IP can follow the workload, which is what makes `dedicated_ip` compatible
with migration rather than an anchor. How each is held is §3.7. A pool of scope `edge` names an
edge group, whose members hold its addresses for `stable` endpoints (§3.1): static on a link the
members share, or moved to the holding member by its router.

> Moved 2026-10-26, when dedicated addresses were designed (§3.7): the pool was defined here as a
> block of fields with no owner, no node, no router, and no quarantine, and who owns a pool, where
> its addresses go, and when one is free again had no field to be decided by. Its definition is
> now in `01-model.md`, the owner of the model's nouns, with those fields.

### 4.1 A workload's network on a node, as built

Decided 2026-10-15, for the networking of Phase 1 (§4 of `20-roadmap.md`), which runs on one
machine; nothing here prevents the overlay of §9 from joining these namespaces later.

> Corrected 2026-10-25 (Finding 173 of `23-walkthroughs.md`): the sentence above was not so. Every
> node's default pool below is the same `10.47.0.0/16`, and the mesh routes rather than
> translates, so it cannot carry one prefix to two nodes. §9.1 gives each node a pool of its own.

**The agent decides, the helper executes, the driver joins.** A workload's network namespace is
data-plane state the agent owns (§2 of `02-architecture.md`). From the assignment the agent
computes what the workload's network must be: its addresses, its declared endpoints, its allocated
ports, and its policy. The helper, which is root, makes it so with two operations, `EnsureNetwork`
and `ReleaseNetwork`, idempotent and keyed by workload, as it does for volumes (§3 of
`06-storage.md`). The driver joins the namespaces in `/run/korpis/netns/<workload>`, its network
namespace and the user namespace that owns it (below), and declares `network_modes: veth` (§4.1
and §7 of `04-runtimes.md`); it builds no networking of its own. The `firecracker` driver declares
`tap` instead: its guest holds the workload's addresses behind a tap in the same namespace (below).

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
to the node: the /22 the control plane gave it when it enrolled (§9.1), or `10.47.0.0/16` for a
node enrolled before pools were given, unless the operator names another with `korpis-agent run
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

> Corrected 2026-10-25 (Finding 175 of `23-walkthroughs.md`). The node reported the address only
> inside each reachable endpoint's status, so a workload with no reachable endpoint never had its
> address reported, and a dependent with none could not have been admitted on its target's node
> (§9.1). The observation now carries the address itself, `Observation.address`.

**Inbound is closed in the namespace itself.** The namespace has its own nftables table, whose
input policy is to drop: it accepts its loopback, the replies to connections it opened, the ICMP
and neighbour-discovery messages a network needs, and new connections only to the ports of its
declared endpoints, over their protocols. It is written by the helper with the rest, so an
endpoint removed from the intent closes its port at the next assignment. A rule in the workload's
own namespace holds for traffic from anywhere, the node and its other workloads included, which
is what §4's "closed except where declared" requires. For a microVM's guest the same rules are on
the namespace's forward hook, which the guest's connections cross (below).

**A microVM's guest holds its addresses behind a tap.** Decided 2026-10-23, as the `firecracker`
driver was given its network (§6 of `04-runtimes.md`). A guest's interface is its own kernel's, so
the workload's addresses are set inside the guest, from its config drive, and the namespace keeps
the link. The agent asks for the network with a tap when the workload's driver declares `tap`
(what each driver declared at its start, §4.1 of `04-runtimes.md`). The helper then makes `tap0` in
the namespace beside `eth0`, persistent, so it outlives the VMM, and owned by the workload's first
id, which the jailer runs Firecracker as, so no other user of the host can attach to it. The
namespace routes between the two as the host routes between the link and its uplinks: `eth0` holds
no address of the workload's, the workload's addresses are routed to the tap, and the default route
is the gateway on `eth0`. The namespace answers for each side on the other, by proxy ARP on both
links and proxy NDP entries for the workload's IPv6 address and the IPv6 gateway, so the host sees
the guest where it saw a container's `eth0`, the guest sees `169.254.1.1` and `fe80::1` as a
container does, and neither learns that a hop stands between them. Everything on the host side is
unchanged: the link, its routes, its rules (§6), its shaping and accounting (§7) and the node's
forwarded ports (§3) are the container's, since they name the link and the workload's addresses and
never the interface behind them. The namespace has no address on either link, so it reaches the
gateway through a permanent entry for the host side's hardware address; that address is set by the
helper when it makes the link, from the hash the link's name is made from (Finding 148 of
`23-walkthroughs.md`). Its own firewall moves to the forward hook: the guest's connections out
are let through, replies are let back, and new connections in reach only the declared ports; its
input admits from the tap only queries to the resolver's address (§5.1). A guest is root in its
own kernel, so it can send from any address, as a container, holding no `NET_ADMIN` or
`NET_RAW`, cannot: what comes from the tap is therefore taken only from the workload's own
addresses and link-local ones, which are never forwarded, and dropped and counted otherwise,
before any other rule; and the namespace's IPv4 reverse-path filter is strict besides. A namespace
made for one kind of interface is made again for the other, since a tap is not a veth's end. What
it costs: one more routing and firewall lookup per packet, in the namespace; and the namespace
forwards, which its own `ip_forward`, proxy, and reverse-path settings allow, sysctls of that
namespace alone, never the host's.

> Corrected 2026-10-23, from a review of the first build (Finding 149 of `23-walkthroughs.md`):
> this paragraph said a guest's other addresses reached nothing, since the namespace routes only
> the workload's to the tap and the host only those to the link. That holds for what is sent to a
> guest, not for what it sends: the namespace forwarded from the tap whatever the source, and the
> host's loose reverse-path filter passes another workload's address, so a guest could have passed
> for another workload, or had its resolver answer another address.

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
for Korpis's links, which are all named `kw` and thirteen more characters (corrected 2026-10-23:
this said twelve; the name is fifteen bytes, the most an interface's may be). Until the operator
opens
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
/run/korpis/deps/db.json                      (file, for workloads that reload)
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
dependency was refused as not yet available (§4 of `20-roadmap.md`); they were built 2026-10-15, as
below.

**As built.** Decided 2026-10-15, for Phase 1, which runs on one node. Each part of a dependency is
evaluated where the information it needs is held.

*Scope.* A dependency's target is in the dependent's own project, by identifier or by selector. A
declaration naming a workload or an endpoint of another project is refused: reaching another
project's workload is a grant of its own, which no action yet expresses (question 9 of §11).

*What a target means.* Each target resolves to endpoints. A workload resolves to its one endpoint
whose exposure is not `none`; a workload with none, or with several, is refused when the dependency
is declared, naming the endpoint form, since nothing could say which was meant. An endpoint
resolves to itself. A selector resolves, each time the dependent's assignment is computed, to every
workload of the project it matches, each by its one reachable endpoint; a matched workload with
several is left out, and the dependency's status says so. The control plane evaluates all of this,
since it holds the intents, the labels, and the endpoints, and carries the result in the
dependent's assignment: for each dependency its name, how it is injected, whether it is required,
and each target's workload, endpoint name, container port, and protocol.

> Corrected 2026-10-25 (Finding 176 of `23-walkthroughs.md`). No dependency status existed: a
> matched workload with several endpoints was left out silently, and nothing anywhere said so.
> The workload's status now carries each dependency (`WorkloadStatus.dependencies`): each target,
> how the dependent reaches it (on its own node, through the mesh, unreached, or not placed, the
> nodes not named, since whoever reads the workload may not read them), and each workload it names
> and leaves out, with why; `korpis status` prints it. It is computed when the workload is read,
> from the resolution, both placements, and the mesh's pairs.

*Where it is placed.* Until the overlay exists (Phase 3) no node reaches another's workloads, so a
dependent is placed on the node its targets are on, which the filter row of §2.1 of
`05-scheduling.md` already states for a node outside the overlay. A required dependency whose
target is not placed leaves the dependent unplaced, saying so; one that is not required constrains
nothing. Targets on two different nodes leave it unplaced: there is no node from which both are
reachable. With the mesh (§9.1, designed 2026-10-25), a target on another node is reached through
it when the pair is reachable, and the dependent may be placed on any node from which every
target's node is. Built 2026-10-25 (§9.1); until then the rule above held.

*The address.* The node turns each target into an address: the target's workload address, which its
agent allocated and alone holds (§4.1), and the endpoint's container port. A target that is not on
the dependent's node is not resolved. With the mesh it is, and not by the dependent's node, which
holds no other node's addresses: the control plane, which learns each workload's address from its
node's observations (§4.1), carries a remote target's address in the dependent's assignment, and
it is reached at that address, since every node's pool is its own (§9.1). Built 2026-10-25.

*Injection.* Into the environment at the container's creation: `KORPIS_DEP_<NAME>_HOST` and
`_PORT`, of the first target in the order of the targets' names, and `_ADDRESSES`, every target as
`host:port`, comma-separated. Into a file, `/run/korpis/deps/<name>.json`, in the workload's own
directory that the identity socket's is (§9.5 of `08-identity.md`), mounted read-only: every
target, with its workload, endpoint, host, port, and protocol, rewritten whole whenever it changes,
so a workload that watches it follows a change without a restart. The environment changes only at
the next start. A dependency that is not required and has no target now is injected as none: no
variables, and a file with no addresses.

*Reaching it.* The egress default denies other workloads (§6), so the agent opens each target's
address and port, over its protocol, in the dependent's own chain on the host, as it opens the
control plane's API for an extension's workload. The target's own table accepts its declared port
already, and its endpoint's own rules still hold (§6). The connection is forwarded by the host from
one workload's link to the other's, so a firewall of the operator's that drops forwarded traffic
drops it too until it accepts Korpis's links (§4.1); each such rule counts what it lets through,
under the comment `dependency`.

*Start order.* The agent creates the dependent's container only once every required dependency's
targets on its node run and, where they have a readiness probe, are ready; until then the dependent
is `starting`, and its status says which dependency it waits for. The agent judges this, since it
observes both workloads.

> Corrected 2026-10-23, when probes were built (Findings 151 and 152 of `23-walkthroughs.md`).
> Readiness was not judged by any node until then, so a target with a readiness probe was taken
> as ready once it ran (§5 of `04-runtimes.md`). And the wait held only where the node created the
> dependent by itself: a plan's `create` or `start` step, which is how a declared dependent is
> first created, made the object whether its dependencies ran or not, so the order held for a
> dependent created again after a stop and not for a new one. Both steps now wait, the step left
> outstanding and the status saying for which dependency, as the node's own creation does; the
> wait is for a target's readiness probe where the node runs it, and for its running where it
> does not.

What it costs: a dependent and its targets share a node until the overlay exists, so a database
that fills its node keeps its web application off every other; a change of address reaches the
environment only at the next start; and a selector is evaluated when the assignment is computed,
not continuously, so a workload that starts to match joins at the next computation, which any
change on the node or the dependent's target triggers. What it does not do yet: placement keeps a
dependent beside its targets, and not a target beside its dependents, so a target placed after a
dependent that does not require it is placed by its own needs alone, and on a fleet of several
nodes the two can land apart and the dependency go unreached until the dependent is placed again.
On one node, which Phase 1 is for, they cannot; the overlay of Phase 3 ends the question, since
then nothing needs them together.

> Corrected 2026-10-15, when it was designed. The file was `/korpis/deps/<name>.json`: a directory
> of its own at the root, which needs a mount of its own. The workload's Korpis directory,
> `/run/korpis`, is mounted already and is Korpis's own, so the files are there, and no host path
> is mounted that was not before.

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
- The resolver rate-limits queries per workload, because a resolver open to every tenant is a
  resource every tenant can exhaust. This bullet also said the resolver's address was the one
  exception the policy carries by default; as built it is none, since the resolver is in the
  workload's own namespace and nothing reaches the node (corrected below).

**As built.** Decided 2026-10-15. The resolver's address is `169.254.1.53`, an address of the
workload's own namespace's loopback, and the `/etc/resolv.conf` its container is given names it
alone; the file is mounted read-only. A microVM's guest is given the same address on its config
drive, and `korpis-guest` writes the file into its root's writable layer before the root is made
read-only, in place of whatever the image held there; the guest reaches the address through its
tap, the namespace's input admitting from the tap nothing else (§4.1). The helper opens a UDP
socket and a listening TCP socket on that address's port 53 inside the namespace, and hands them to
the agent with the answer to its request (`OpenResolver`, the descriptors attached as
`SCM_RIGHTS`); the agent answers on them from the host, as a socket keeps the namespace it was made
in. A query never leaves the workload's namespace, so no firewall on the host, Korpis's or
another's, stands between a workload and its resolver; the agent knows which workload asked by the
socket the query arrived on, needs no privilege, and binds nothing on the host. It forwards each
query, over UDP or TCP as it came, to the resolvers in the host's `/etc/resolv.conf`, which the
host reaches whatever they are, a local stub included, and allows each workload 50 queries a second
with a burst of 200, answering beyond that with `REFUSED`, which a resolver library treats as a
failure to retry rather than as an answer, and an upstream that does not answer in two seconds with
`SERVFAIL`. An agent that restarts asks for every running workload's sockets again, and a
workload's are closed when its network is released, since they keep its namespace alive. The
dependency names of §5 are answered here once dependencies are resolved (§5). What it costs: a
workload whose software ignores `/etc/resolv.conf` and asks a resolver of its own on the public
internet still reaches it, as egress allows; one that asks a private resolver is denied by §6, as
it should be; and the agent holds two descriptors for each workload it runs.

> Corrected 2026-10-15, when it was built (Finding 56 of `23-walkthroughs.md`). The first version
> made the resolver the workload's gateway, `169.254.1.1`, and redirected its port 53 on the host
> to a port the agent listened on. A redirected query is delivered to the host, and every
> firewall's input chain judges it, not only Korpis's: on a host whose firewall drops input by
> default, as `ufw` does, no workload could resolve a name until its operator opened a port, and
> Korpis does not change another firewall. That version also needed the one exception to the
> default of §6 that the third bullet above names; this one needs none, since nothing reaches the
> host.

---

## 6. Policy

No panel in this market has egress control, and it is the gap that hurts operators most, because a
compromised game server is a launchpad.

```
NetworkPolicy                   on each endpoint
  ingress   []Rule              who may connect to it; the first rule that matches decides,
                                and a connection no rule matches is let in
  conntrack ConntrackLimits     connections to it at once, and new ones a second

EgressPolicy                    the workload's own, IntentBody.egress
  rules     []Rule              where it may connect out, before the defaults below
  limits    ConntrackLimits     each may lower the default, and not raise it
```

> Corrected 2026-10-15, when a workload's own policy was built (Finding 55 of
> `23-walkthroughs.md`). The policy was one object on each endpoint, with outbound rules beside the
> inbound ones, and a node judging a packet the workload sends has no endpoint to read a rule from:
> a connection a workload opens leaves from no endpoint, so the outbound rules of two endpoints
> that disagreed had no meaning any component could evaluate. The outbound rules and limits are the
> workload's own, `IntentBody.egress`; an endpoint's policy says who may connect to it and how many
> at once, which its node can judge, because a connection to an endpoint arrives at its port. The
> endpoint's `egress` field stays in the frozen schema (rule E of `20-roadmap.md`) and a
> declaration that sets it is refused, naming where the rules now go. What it costs: a workload
> with one endpoint loses nothing, and one whose endpoints wanted different outbound rules was
> asking for something no packet filter could hold.

**Egress defaults, applied to every workload unless deliberately changed.** This table is the
owner of the egress default. `17-security.md` §2 and §10 once described egress as "default-deny";
the default is the table below, deny for every internal destination and allow, rate-limited, for
the public internet (finding S44):

| Destination | Default | Why |
|---|---|---|
| Other tenants' workloads | **deny** | lateral movement between customers |
| Node management interfaces, SSH, the agent socket | **deny** | privilege escalation path |
| The Korpis control plane's API | **deny** | a workload holding no authority has no business reaching it; an installed extension, and a workload holding a grant in force, reach it through their node, not the network (§9.5 of `08-identity.md`) |
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

> Corrected again 2026-10-15, when an extension was first run in a container (Finding 57 of
> `23-walkthroughs.md`). The exception opened the API's address as the node reaches it, and on a
> single machine, the installation Phase 1 is for, the node reaches it on loopback, which a
> workload's namespace never reaches; any other address of the host meets every firewall's input
> chain, which an operator's may close. No extension could reach the control plane on the
> installation it was most likely to run on. An installed extension's workload now reaches the API
> through its node: the agent relays a socket in the workload's own directory to the control plane
> over the host's network, and the extension speaks TLS through it to the control plane itself
> (§9.5 of `08-identity.md`). Nothing is opened past the default for it, and the row is a deny for
> every workload again.

> Corrected again 2026-10-19, when a scheduled run was first to take a backup (Finding 103 of
> `23-walkthroughs.md`). The first correction above left "any other workload that is granted
> something" to open the API's address in its own policy, and the second showed that address
> unreachable on the installation Phase 1 is for, so a workload issued a grant, a nightly
> backup's above all, held authority it could not use. The relay is now served to every workload
> holding a grant in force as well as to an installed extension's, decided by the control plane
> from the grants it holds (§9.5 of `08-identity.md`), and the row stays a deny: nothing is
> opened past the default for either.

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
| other workloads | anything the node would forward to another workload's link, whatever pool its address is in; on one node this is every other tenant's workload, and later dependencies open single addresses in it (§5) |
| node management | anything addressed to the host itself, on any of its addresses, except ICMP to the gateway; the resolver is in the workload's own namespace (§5.1), so it needs no exception |
| the control plane's API | nothing is opened for it: an installed extension's workload, and a workload holding a grant in force, reach it through their node (§9.5 of `08-identity.md`); on one machine its address is the node's own, which the node-management row denies, and where it is a public address a workload reaches it as any public address, which its authentication alone bounds |
| link-local metadata | `169.254.0.0/16` and `fe80::/10` beyond the gateway, `fd00:ec2::254` among them |
| RFC 1918 and unique local | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `100.64.0.0/10`, `fc00::/7`, and loopback |
| SMTP | TCP 25, 465, and 587, to anywhere |
| the public internet | everything else: allowed, at most 200 new connections a second with a burst of 400 |

The conntrack limits are per workload: at most 4096 tracked connections, in both directions, and
the new-connection rate above. Half-open connections have no counter of their own: the new
connection rate bounds how fast they can be made, and the node turns on SYN cookies, so a flood of
them costs the conntrack table nothing. Between two workloads on one node both ends hold (§9.1 of
`17-security.md`): the source's chain on the host and the destination's own namespace. A workload's
own policy is evaluated before the defaults. Its deny holds anywhere. Its allow holds only
for the public internet and, through a dependency (§5), for a workload it depends on: the rows that
protect the node, the operator's networks, the metadata service, other tenants, and the reputation
of the operator's addresses (SMTP) are the operator's to relax, not a tenant's, and nothing relaxes
them until an operator-level policy exists (question 7 of §11). What it costs: the limits are a
guess at what a game server or a web service needs, and one that opens more connections than that,
a crawler or a proxy, needs its policy to say so; and as §9.1 of `17-security.md` says, a node
whose root is compromised enforces none of this.

**Built.** Added 2026-10-15. The rows are two base chains of the host's table, on the `forward` and
`input` hooks, that judge what a workload's link sends; each row that denies counts what it drops,
under a comment naming the row, which is how the tests tell this firewall's verdict apart from
another one's on the same node, and what accounting (§7) will read. The chains shared by every
workload are written once per version of their definition, marked in the table, and left alone as
each workload's network is made, so their counts accumulate; the first version rewrote them for
every workload, which zeroed every count, and a test racing another workload's start was what
showed it. They run at priority -10, before the filter chains at the default priority, an
operator's firewall among them, so what they deny is dropped early whatever another firewall
decides; what they let through, another firewall can still drop (§4.1). A connection to a published
endpoint is a client's, whoever makes it, so one a workload makes to another's endpoint through the
node's address passes as any client's does, and the destination's own table judges it (§3). The
rate limit is on the new connections a workload makes, to the internet or to the node; connections
made to it are bounded by the tracked-connection limit, counted per workload address in both
directions, and not by a rate, since a game server's players arrive in bursts its operator chose to
serve. What differs between workloads, the destinations its dependencies open (§5) and the rate's
state, is a chain of the workload's own, reached through a map keyed by its link. The workload's
own policy is the next paragraph.

**A workload's own policy, as built.** Added 2026-10-15. The workload's rules are in its own chain
on the host, after the destinations opened for it and its limits and before the rows of the
default: a deny drops the connection and counts it, under the comment `policy`; an allow returns it
to the default, which decides the rest, so an allow cannot open what the default protects. Its
limits may lower the default's 4096 connections and 200 new a second and not raise them, since the
default is what keeps one workload from taking the node's connection table from the others; a
declaration that raises one is refused, and raising them is the operator-level policy of question 7
of §11. An endpoint's rules are in the workload's own table, in a chain for the endpoint's port
that the port's connections pass through: its limits first, a connection limit and a new-connection
rate for the endpoint as a whole, then its rules in order, an allow letting the connection in and a
deny dropping and counting it, and then anything no rule matched, let in. They see the client's own
address, which the node's forwarding preserves (§3.4). The node's own health probes do not pass
through them: each is made from a socket opened inside the workload's namespace, which reaches the
endpoint as its loopback does and so is judged by none of its rules or limits (§5 of
`04-runtimes.md`), since a probe an endpoint's allow list turned away would restart a healthy
workload. Every rule names an address range, because a packet filter judges addresses: a rule
naming workloads by label is refused until Phase 3, since on one node another workload is denied by
the default and opened by a dependency (§5), so such a rule would decide nothing those do not, and
it is needed where workloads on other nodes are reached, which the overlay brings (corrected
2026-10-15, when dependencies were built: it had been promised with them, and they resolve
endpoints, not a policy's peers), and one naming a domain is refused until question 8 of §11 is
answered. A rule that arrives at a node anyway fails the workload's network rather than being left
out, because a deny left out is a weaker policy than the one declared. Half-open connections have
no limit of their own to declare (above), and a declaration that sets one is refused. What it
costs: an endpoint's limits are for the endpoint as a whole and not for each client, so they
protect the workload from a flood and not one player from another, which is the workload's own to
judge.

> Corrected 2026-10-26 (Finding 182 of `23-walkthroughs.md`), when rules naming workloads were
> designed. The paragraph above deferred them because on one node another workload is denied by
> the default and opened by a dependency, so such a rule "would decide nothing those do not", and
> because the overlay would make them necessary. Asked what such a rule decides, both halves
> fail. A dependency opens the dependent's way out of its own chain, and what arrives at the
> target's endpoint is still judged by the endpoint's rules (§5), so a rule naming workloads on
> an endpoint decides which of the workloads that may reach it are let in, on one node as on
> many. And the mesh admits from another node only the dependents of a node's workloads (§9.1),
> as one node's default does, so the overlay made nothing newly necessary. What it changed is
> where a workload's address is known: no longer on its own node alone.

**Rules naming workloads, as designed.** Decided 2026-10-26, for Phase 3 (§4 of `20-roadmap.md`).
An endpoint's rule may name workloads by a label selector, to allow or to deny. A workload's
egress rule may name workloads only to deny: an allow returns a connection to the default, which
denies every other workload, so it could open nothing, and is refused at declaration naming the
dependency (§5), which is what opens one workload to another. An egress deny naming workloads
decides what the default does today, goes on deciding it where an operator's default opens
private ranges (question 7 of §11), and counts what it drops under `policy`; a deny naming a
workload the declaring workload depends on does not hold against the dependency, which is opened
before its rules (above), since the workload declared both. The selector matches workloads of
the rule's own workload's project, as a dependency's does (§5), and a rule naming another
project's is refused, since nothing yet reaches across projects (question 9 of §11).

*What it names.* A packet filter judges addresses, so the control plane resolves each such rule,
when it computes the assignment of the rule's workload, to the addresses of the workloads it
matches, as their nodes observed them (§4.1). An address is named only where the rule's node
would route a packet for it to the matched workload: a workload on the rule's own node, at its
addresses there, and one on a node paired with the rule's in the mesh, at its addresses in that
node's routed pools, which are disjoint from every other member's (§9.1). A matched workload
anywhere else is left out of an allow, since its private address may be one the rule's node gave
a workload of its own (two nodes on the old default both hold `10.47.0.0/16`), and naming it
there would let in another workload, perhaps another tenant's. A deny names, besides, every
global IPv6 address a matched workload is observed at, since a workload with one reaches an
endpoint exposed at a global address from anywhere; naming it there drops more and never lets
more in. So the node observes a workload's IPv6 address as it does its IPv4 one, which until now
it did not.

*How the node holds it.* The assignment carries each such rule's addresses, which may be none.
The agent hands them to the helper as the rule's addresses, and the helper writes the rule over
that set for each family that has any; a rule naming no address now is written as none, which
matches nothing, which is what it means. A rule naming workloads that arrives without its
addresses fails the workload's network, as any rule a node cannot hold does.

*When it changes.* A workload of the project placed, retired, relabelled, or observed at a new
address wakes the node of every workload of the project whose rules name workloads, which is
told its rules' addresses again, and the agent makes a running workload's network again when what
it would ask the helper changes (Finding 181 of `23-walkthroughs.md`). Until it is told, a rule
holds the addresses it was given last: a workload newly matched is neither let in by an allow
nor dropped by a deny, and an address a matched workload left is judged as its, even if its node
gives it to another workload meanwhile. That workload is still held by its own node's default and
by the mesh's admission, which open nothing to it but its own dependencies. While the control
plane is unreachable the rules keep their last addresses, as dependencies keep theirs.

*What it shows.* The workload's status carries, for each such rule, the workloads it matched and
those it left out, with why, as a dependency's status does (§5).

What it costs: each change to a project's workloads recomputes the assignments of those of its
workloads whose rules name workloads, so a project of many such workloads that changes often
costs passes in proportion, and the addresses ride in every such assignment. What it does not do:
it judges a connection by its source address, so it matches a workload only where the connection
arrives from that workload's own address, which is through a dependency, on one node or through
the mesh, or over a global IPv6 address. A connection that reaches the endpoint through the
ingress proxy arrives from the proxy, and one through another node's port arrives translated to
that node's address; no rule naming workloads matches either.

> Built 2026-10-26. The control plane resolves each rule when it computes the assignment
> (`Assignment.peers`), from the nodes' observations, which carry a workload's IPv6 address
> beside its IPv4 one (`Observation.address_ipv6`), and a change to any workload of a project
> wakes the nodes of the others whose rules name workloads. The agent hands each rule its
> addresses, and the helper holds a rule for each family it names and none where it names none.
> The declaration accepts a rule naming workloads on an endpoint, to allow or deny, and on the
> egress, to deny, and refuses an egress allow naming them, naming §5. The status carries what
> each rule matched and left out (`WorkloadStatus.named_rules`), and `korpis status` prints it.
> Tested in `korpis-core` (a workload's own node's addresses, a paired member's routed pools, a
> node on the old default left out of an allow, and a deny's global address), over the API (a
> database's rule resolved to a web application's address on the other node of the mesh, and its
> status), in the agent and the helper (never left out; a rule for each family), and end to end
> on one node: a web application the rule names reaches its database, and a report that depends
> on it does not.
>
> Corrected 2026-10-26 (Finding 183 of `23-walkthroughs.md`, found by that end-to-end test). A
> rule naming every address of a family, `0.0.0.0/0` or `::/0`, which is how an endpoint says
> "and no one else" after its allows, was accepted at declaration and refused by every node: the
> helper read a rule's range as it reads a pool, and a prefix of length zero is no pool. The
> workload's network failed and it never ran, whatever the rest of its rules. The helper now
> reads a rule's range as a range, which may be every address of its family; a pool still may
> not be. Nothing else in the declaration and the node's reading of a rule differed.
>
> Corrected again 2026-10-26 (Finding 184 of `23-walkthroughs.md`, from an automated security
> review of the commit that built it). A matched workload was named at the addresses its node
> last observed, whatever it was doing: a workload observed stopped, crashed, or unsatisfiable
> still had its last address named, though nothing promised its node had not let that address go.
> A matched workload is now named only while its node observes it starting, running, or stopping,
> when its network is made and it may send, and is otherwise left out as not observed. What it
> costs: a deny drops nothing from a workload while it is not running, which then sends nothing.

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

**Not yet built, as first written; both are built since, in Phase 2.** Phase 2 builds shaping and
accounting, beside the metering accounting feeds (§4 of `20-roadmap.md`). Until then a declaration
that asks an endpoint for either is refused, and a workload whose `bandwidth` sets a reservation or
a limit is placed only on a node whose driver declares it enforces one, so it stays unplaced with
that reason where none does (§4.1 of `04-runtimes.md`, §5.3 of `05-scheduling.md`): either way
nothing is shown as held that the kernel does not hold (Rule K-3). Until the node's shaping was
built (below) no driver did; now a node with a declared uplink does. Since an endpoint's own
shaping and accounting were built too (P2.8c, below), a declaration asking for either is no longer
refused: it is placed only on a node whose driver declares `endpoint_shaping` or
`endpoint_accounting`, and stays unplaced with that reason where none does.

**Shaping and accounting, as designed.** Designed 2026-10-21, before they were built (Phase 2 of
§4 of `20-roadmap.md`), on the network of §4.1: a point-to-point link per workload, routed, every
packet crossing the host's table.

*Where a reservation is held.* A guarantee is only a guarantee where the guaranteed contend, and
outbound the workloads of a node contend for its uplinks. So the reservation is held there: on each
uplink the helper keeps an HTB tree whose root rate is the uplink's capacity, a class for each
workload with an egress reservation, its rate the reservation and its ceiling the limit (as
corrected when built, below), and a default class for everything else, the node's own traffic and
the workloads that reserve nothing, whose rate is what the reservations leave and whose ceiling is
the capacity. A packet is put in its workload's class by the host table: the workload's chain,
reached from the verdict map keyed by its link, sets the packet's priority to the class in the
forward hook, after the kernel's forwarding has set a priority of its own and before translation,
which does not touch it, and HTB takes a class from a packet's priority with no filter. A priority
rather than a firewall mark, because marks are shared with every other program on the node that
routes by one, and a priority set in the forward hook is Korpis's alone. Each class's identifier is
allocated by the agent with the workload's addresses, kept across restarts, and released with them.

*The uplinks and their capacity are declared.* `korpis-agent run --uplink eth0=1gbit`, repeatable,
names each interface and the capacity it sells, because the link speed the kernel reports is the
negotiated rate of a NIC, and on most virtual machines a fiction; the capacity a host sells is what
its provider sold it, which only the operator knows. The node keeps part of each uplink for itself,
five percent unless the flag names another (`eth0=1gbit,reserve=50mbit`), as the default class's
floor, so its control stream, journal, and relays are never starved by reservations it sold. With
several uplinks each gets the same tree, and the node's allocatable bandwidth is the smallest
uplink's capacity less what it keeps, since a workload's traffic leaves by whichever its route
chooses. A node that declares no uplink shapes nothing and enforces nothing.

*The limit is held at the workload's own link.* The ceiling a workload is sold covers all it
sends, and not all of it leaves by an uplink: traffic to the node itself, its ingress proxy
included (§3.2), never reaches one. So the limit is also shaped on the workload's side of its link,
an HTB qdisc on `eth0` in its namespace, which the helper writes and the workload cannot change,
since it holds no `NET_ADMIN` (§4.1). The uplink's tree holds the guarantee among what leaves by
it; the link's qdisc holds the ceiling on everything.

*The ingress limit is shaped on the host's side of the link.* What the node sends a workload
leaves the host's side of the link, so an HTB qdisc there holds `bandwidth.ingress.limit`, on
everything the workload is sent, from beyond the node, from its proxy, and from other workloads.

*What each endpoint carried is counted in the host table.* Every packet to or from a workload's
link passes the host's table, forwarded, sent by the node, or received by it, and the workload's
chains count, for each declared endpoint, the bytes and packets to its port over its protocol and
from it, as named counters: in the host's namespace, out of the workload's reach, which is the
point of counting in the kernel rather than in a proxy. The chains also count what leaves the
workload's link for anything but another workload's link on the node: the workload's egress, which
the egress budget is measured in. The meter reads the counters at each of its readings through the
helper, one listing of the table, and journals them with the interval's sample, so they are
metered as everything else is (§2 of `15-observability.md`), cumulative and differenced.

*The node says it enforces them.* A node whose helper can write the qdiscs and whose operator
declared an uplink adds `egress` and `ingress` to what its drivers that join its links enforce
(§4.1 of `04-runtimes.md`), so a workload with a bandwidth reservation or limit is placed only
there, against the node's allocatable bandwidth, never oversubscribed (§5.2 of
`05-scheduling.md`). A node without them keeps refusing, with that reason.

What it does not do, and what it costs. Traffic between two workloads of one node never reaches an
uplink, so it is held by the sender's ceiling and the receiver's ingress limit and by no
guarantee. The overlay, when it is built (§9), is the node's own traffic to its uplink and goes in
the default class until it carries the class of the workload inside it. A reservation is a
guarantee at the uplink's interface and says nothing beyond it (above). HTB's precision is that of
the kernel's timers, a few percent at gigabit rates; a ceiling is a rate, so a burst within the
qdisc's buffer passes at line rate before it is held. The counters cost a rule per endpoint per
direction on every packet of the workload, and a listing of the table every ten seconds, which for
a thousand workloads is a few hundred kilobytes of JSON. The node needs `tc` from iproute2, as it
needs `nft` (§2 of `18-operations.md`).

> Built 2026-10-21 on the node (P2.8b of the build plan): the helper's `EnsureUplinks` makes each
> declared uplink's tree and removes Korpis's tree from an interface no longer declared, and
> `EnsureNetwork` takes a workload's bandwidth (`crates/korpis-helper/src/network/shape.rs`,
> `account.rs`, `bandwidth.rs`). Every qdisc Korpis makes has the handle `4b50:`, so its trees are
> told apart from another program's and found again with no ledger; each class sends in turns of
> 60,000 bytes, where HTB's own quantum, a rate over ten, is megabytes at gigabit rates; and each
> leaf queues by `fq_codel`. The classes are allocated by the helper from a map in the host table,
> keyed by the workload's link, so it finds a class again to remove it. Three base chains on the
> forward, input, and output hooks, at priority -150, send each packet from or to a workload's
> link to its own chains, which count and set the priority and decide nothing but a cut-off; the
> counters are named `<link>_egress` and `<link>_<transport>_<port>_in` and `_out`, and survive a
> rewrite of the chains. `korpis-agent run --uplink` declares the uplinks, the agent shapes them
> at start, and only then adds `egress` and `ingress` to its link-joining driver's enforcement and
> its allocatable bandwidth to its facts; `korpis-agent preflight` reports `tc` and HTB as the
> helper finds them. Corrected the day it was built: the design gave a class on the uplinks to
> every workload with an egress reservation or limit. Only a reservation needs one, since a class
> is how a guarantee is held among the contending; a limit alone is held on the workload's own
> link, which every byte it sends crosses, and its uplink traffic goes in the default class. The
> helper's test runs in a network namespace of its own, standing for the host, with a dummy
> interface as its uplink, and finds the tree, the class with its rate and ceiling, the forwarded
> packets in it, both link limits, the counters of a connection to the endpoint and of what the
> workload sent, and all of it gone when the workload stops reserving and is released. So a node
> with a declared uplink now enforces a workload's bandwidth and offers it to placement, which
> reserves it as it reserves the other guaranteed dimensions (§5.2 of `05-scheduling.md`); a
> node without one still enforces none, and a workload that sets one is not placed there. Not yet
> built: placement tested against a node that declares an uplink, and an endpoint's own bandwidth
> (P2.8c), the meter's reading of the counters (P2.8d), and the egress budget (P2.8e).

> Corrected the day it was built (Finding 125, §47 of `23-walkthroughs.md`): the note above gave
> every qdisc Korpis makes the handle `4b50:` and found the uplinks' trees again by it, so the
> qdiscs on the workloads' links were found as uplinks too. Declaring the uplinks removed each
> workload's ingress limit, and a reservation's class was written on every other workload's link. A
> qdisc on a workload's link now has the handle `4b51:` and an uplink's `4b50:`, and the uplinks'
> trees are looked for only among interfaces whose names are not a workload link's.

> Built 2026-10-21 (P2.8c of the build plan): an endpoint's own bandwidth and its accounting. An
> endpoint's `shaping` limits what it sends from its port and what is sent to it, each in a class
> of its own beneath the qdisc of the workload's link. On `eth0` the class is chosen by the
> priority the workload's own table sets on what leaves the port, in an output chain the workload,
> holding no `NET_ADMIN`, cannot change; a priority it sets on a socket itself can put its traffic
> only in another of its own classes, each beneath its own limit. On the host's side the class is
> chosen by the priority the workload's `to_` chain sets on what goes to the port. That chain sets
> the priority of everything sent to the workload, `none` where no endpoint's class is its, so a
> priority the sender's chain set for its uplink class never lands in one of this workload's
> classes. A link's qdisc is now a root of the workload's limit (of no limit, where only its
> endpoints set one), a default class for everything else, and the endpoints' classes, each with an
> equal share of the root as its rate and its own limit as its ceiling, so the endpoints contend
> fairly beneath the workload's ceiling; a class an endpoint no longer needs is removed. Corrected
> from the design above, which counted every declared endpoint: the counters are written only for
> an endpoint whose `accounting` is on, since that flag is the declaration that it is metered, and
> a rule on every packet of traffic nobody meters is cost with no reader. The workload's egress is
> counted whatever its endpoints say, since its budget is measured in it. The agent declares each
> kind on what it rests on (§4.1 of `04-runtimes.md`): `endpoint_accounting` wherever the helper
> gives workloads networks, `ingress` and `endpoint_shaping` wherever it can also shape, `egress`
> only with a shaped uplink. Placement is tested against a node that declares an uplink: a
> reservation is placed there within the bandwidth it offers and not beyond it, and on no node
> without one. The helper's privileged test finds both of an endpoint's classes with their
> ceilings, a connection to the endpoint in the host side's class and the reset it is answered with
> in `eth0`'s, and no counter for a port whose accounting is off. Building it found that a priority
> the sender's chain set for its uplink class could put another workload's traffic in an endpoint's
> class on the receiver's link, which is why the `to_` chain decides the class of everything it
> sees (Finding 124, §47 of `23-walkthroughs.md`). Not yet built: the meter's reading of the
> counters (P2.8d), and the egress budget (P2.8e).

> Built 2026-10-21 (P2.8d): the meter reads the counters through the helper's `ReadCounters` at
> each reading, and each record carries its egress and what each counted endpoint carried (§2 of
> `15-observability.md`). Not yet built: the egress budget (P2.8e).

> Built 2026-10-21 (P2.8e): the egress budget, reckoned every minute from the records and held
> by each workload's next assignment, its floor on the workload's link and its uplink class, its
> cut-off in the host table (§5.1 of `05-scheduling.md`). So everything this section designs is
> built.

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
it picks among the free ones, and the node refuses to listen on a port it forwards. Corrected
2026-10-26: a port the node could not listen on, because a process of its machine held it or the
node forwarded it, failed the attempt, and the node waited thirty seconds to be measured, though
the port says nothing of whether the node is reached; the prober now asks at once for the next free
port of the range, up to three, and only an attempt failing on all three waits. It was found by a
test whose node was refused a port another test's node held.

What the probe does not do, stated plainly. It measures from where the control plane runs. A
prober inside the node's network reaches a private address no client outside can, and a prober
on the node's own machine, which is every single-machine installation, reaches the node's public
address without crossing a firewall in front of the machine, such as a cloud provider's security
group. So on one machine `public` means the node answered at that address from inside, and the
operator still owns the outside firewall; the plan that exposes an endpoint says so. A prober
outside every node's network is what an edge tier or a relay on another machine provides (§3.1 of
`02-architecture.md`), and with one the measurement means what a client sees.


### 9.1 The mesh, as designed

Designed 2026-10-25, as Phase 3 builds it (§4 of `20-roadmap.md`, trace 74 of
`23-walkthroughs.md`). Each step names what decides it, where, and with what information.

**Each node has one WireGuard interface, and its key never leaves the node.** The helper makes the
interface, `korpis-overlay`, generates its private key into its own state directory the first time,
and listens on one UDP port, 51820 unless the agent is given `--overlay-port`. WireGuard is in the
kernel from Linux 5.6, below the floor of §2 of `18-operations.md`, and is configured over
generic netlink, as the helper already configures links over netlink (§4.1). The agent reports the
interface's public key and port in its Hello and every heartbeat, beside the addresses it
advertises (§9 above), so the control plane holds each node's key as the node's authenticated
session said it, and never a private key. What it costs: a compromised control plane can name any
key as a node's peer, which gives it nothing it lacks, since it already decides what every node
runs; and the key lasts as long as the node's identity, so it is replaced only when the node is
enrolled again (question 13 of §11).

**Every node's workload pool is its own.** Cross-node traffic is routed, not translated, so that a
workload sees its client's own address, as §3.4 requires of every path. That needs pools no two
nodes share, and every node's default is `10.47.0.0/16` (§4.1). So the control plane, which alone
sees every node, gives each node a /22 of an installation range, `10.48.0.0/12` unless
`korpis-server` is given `--overlay-range`, when it enrolls, and holds it as the node's until the
node is decommissioned. The agent keeps it in its state directory beside its identity, since it
lays out its workloads' networks when it starts, before any session, and uses it unless
`--workload-ipv4` names another; it reports the pool it uses in its Hello. A node enrolled before
pools were given has none kept and goes on with `10.47.0.0/16`, since changing it would renumber
its workloads; and a node whose pool overlaps another member's is outside the mesh, `overlay:
unavailable` saying with which node, until its operator names another. A /22 is 1,022 workloads on
a node, and the default range holds 1,024 nodes. A node's IPv6 prefix is routed alike: its routed
prefix, or the unique local prefix derived from its identity (§4.1), checked for overlaps the same
way. What it costs: an installation that upgrades with two nodes on the old default has one of them
outside the mesh until it is given a pool; the operator is told which, and how.

> Built 2026-10-25 (`korpis-store` `node_pool`, the agent's `network::ipv4_pool`): enrollment
> gives the pool and its answer carries it, asking again returns the same one, removing a node
> frees it, and a range with no free /22 enrolls the node without one, its enrollment's effect
> saying so. The overlap check against operators' own pools comes with the mesh's status.

> Corrected 2026-10-26 (Finding 178 of `23-walkthroughs.md`, from an automated security review of
> the commit that built the mesh). The mesh took each node's pools as its agent reported them,
> checking only that they overlapped no earlier member's, and every member routes a member's
> pools through the mesh: a compromised node could report a prefix no node held, the operator's
> own LAN or half the address space, and have every other node route it, the hosts' own traffic
> included, to itself. A member's pools are now what the control plane gave or can derive: the
> IPv4 pool it gave at enrollment, `10.47.0.0/16` for a node enrolled before pools were given,
> and the unique local IPv6 prefix derived from the node's identity, which the control plane
> computes as the agent does. A node reporting another IPv4 pool, a pool its operator named with
> `--workload-ipv4` among them, or another unique local prefix, is outside, `overlay_outside`
> saying which; an operator's routed IPv6 prefix is left to the operator's network and not
> carried. What it costs: an operator who names a node's pool takes it out of the mesh, and of
> two nodes on the old default the later stays outside until enrolled again, since no pool can be
> given to a node that keeps its workloads numbered.

> Corrected again 2026-10-26 (Finding 179, from an automated security review of that
> correction). A node outside because its pool overlaps an earlier member's was told, and shown in
> `overlay_outside`, which node, whatever organization that node is in. It is now told only that
> its pool overlaps an earlier member's, and that enrolled again it is given a pool of its own;
> who may read both nodes sees their pools. The paragraph above saying "with which node" is
> superseded.

**The control plane tells each node its peers; the peers authenticate the traffic.** Every enrolled
node is a member, whatever organization it belongs to (§3.4 of `01-model.md`), since membership
reaches no workload by itself: a destination accepts only its workloads' dependents (below). Two
members are paired only where they may host workloads of one project: one's organization is the
other's or above it, since a project runs on nodes of its organization and those above it alone
(§2.1 of `05-scheduling.md`); so a tenant's own node is paired with the installation's and never
with a sibling tenant's, and `Node.peers` lists only nodes the caller may read (corrected
2026-10-26, Finding 178). For each member it is paired with, a node is sent the member's public
key, an endpoint (its first advertised address and overlay port, or none when it advertises none),
and its pools, which become the peer's allowed IPs. WireGuard routes a packet to the peer whose
allowed IPs hold its destination, and accepts a decrypted packet only if its source is in the
allowed IPs of the peer whose key decrypted it (§5 of `research/evidence.md`), so a packet arriving
on `korpis-overlay` from a workload address is authentically from the node that holds that
workload's pool. The agent writes the peers, the routes to each peer's pools through the interface,
and keeps them on disk, so the mesh runs with the control plane gone (§4.6 of
`02-architecture.md`); a change is applied whole, a peer removed with its routes.

> Built 2026-10-25, the helper's half (`korpis-helper`'s `overlay`, the `KeepOverlay` verb): the
> interface is made over route netlink and configured over WireGuard's generic netlink family,
> its key kept at `overlay.key` in the helper's state and set only on a link that has none, so a
> link kept across a helper's restart keeps its key. Peers are updated in place on every call and
> only those no longer named removed: replacing them wholesale, as the first build did, ended
> every session on each of the agent's passes, which the test's handshake caught. Tested between
> two network namespaces of the test's own, joined by a veth: each pool reaches the other, a
> source outside the sender's pool is dropped, and a peer no longer named takes its route with
> it.
>
> Built 2026-10-25, the agent's half and the control plane's (`korpis-agent`'s `overlay`,
> `korpis-server`'s `mesh`): the agent keeps the interface every ten seconds and at once when
> told new peers, keeps the peers it was told at `overlay-peers.binpb` in its state directory,
> and reports the key, port, pools, and handshakes in its Hello and heartbeats, which the control
> plane stores on the node as one document. Each replica computes the mesh every two seconds from
> those documents, a node outside it when a pool overlaps one of a node enrolled before it, and
> each session sends its node the other members whenever what it is told changes; `Node.overlay`
> is `available` when every pair with the node has fresh handshakes on both sides. A node that
> gives its workloads no networks keeps no interface and reports none, so it is outside the
> mesh.
>
> Built 2026-10-25, dependencies across the mesh (`korpis-app`'s `workload::across`, the agent's
> `deps`, the helper's `overlay::admit`). Every node reports each workload's address in its
> observation. The control plane gives a dependent's assignment, for each target on another
> member, the address that member observed and whether it is ready there, ready only while the
> pair reaches each other; and gives the target's assignment each dependent on another member
> with that dependent's address. Both are computed when either side's set is, and a change to a
> workload wakes the nodes of its dependents and its targets as well as its own, as a change of
> members wakes every node. The dependent's node opens a remote target in the dependent's chain
> only out through `korpis-overlay`, so a node with no route to it through the mesh sends nothing
> towards the target's address; what leaves into the mesh is not translated. The target's node
> keeps, with the interface, a set of each admitted dependent's address, its own workload's
> address, the transport, and the port, accepts from the mesh only replies and what the set
> holds, counted under `dependency`, and drops the rest, to its workloads and to itself alike,
> counted under `overlay`. It clamps TCP's segment size on what enters the mesh to the route's
> MTU. The scheduler places a dependent on a node from which each placed target's node is
> reachable (§2.1 of `05-scheduling.md`). Tested over the API with two nodes and in two network
> namespaces of the test's own, where a dependent's workload reaches its target's admitted port
> and no other. A dependent's status says, of each target, whether it is reached through the mesh
> or unreached now (§5). `Node` shows its key, port, pools, and each pair with it (§3.4 of
> `01-model.md`). Tested end to end with two nodes whose data planes are each in a network
> namespace of the test's own, joined by a veth: a web application on one fetches its database's
> page on the other through the mesh alone, a port the database's endpoint does not declare is
> not reached, and what the first node sends from an address of its own pool that is admitted for
> nothing is dropped and counted (trace 74 of `23-walkthroughs.md`).

**A pair is reachable when both its nodes say a handshake is fresh.** A node behind NAT advertises
no address (§9 above), so it has no endpoint; it reaches a peer that has one, keeping the NAT
mapping open with a keepalive every 25 seconds, and the peer learns its endpoint from the
authenticated packets. So a pair works when at least one of its nodes is reachable, and two nodes
that both are not cannot reach each other, as position 3 above says, with no relay. Each node
reads, for every peer, the time of its latest handshake, and reports it with its heartbeat. A
session older than 180 seconds carries no data (WireGuard's Reject-After-Time, §5 of
`research/evidence.md`), and a session in use is renewed every 120 seconds, which the keepalive
keeps in use; so the control plane takes a pair as reachable when both nodes' last reports name a
handshake with the other younger than 180 seconds. `Node.overlay` is `available` when the node
reaches every member it is paired with, and the scheduler asks the pair (§2.1 of
`05-scheduling.md`): a dependent may be placed on a node from which each target's node is
reachable. What it costs: a pair is judged at most one heartbeat late, so a link that has just
failed takes placements for up to that long, and the dependency's status says it is unreached once
the node reports it so (built 2026-10-25, §5).

**Both ends hold the policy**, as §9.1 of `17-security.md` decided east-west traffic would be: the
source drops early and the destination does not trust the source. The dependent's node opens the
target's address and port in the dependent's own chain, as on one node (§5). The target's node does
not take that on trust, since a compromised node could send anything its interface carries. So the
control plane, which resolved the dependency, gives the target's node each remote dependent's
address and the endpoint it may reach, and the target's node accepts a packet from `korpis-overlay`
to a workload only from a dependent of it, to a port of the dependency's endpoint, counting what it
lets through under the comment `dependency`. Nothing else crosses the overlay into a workload: a
workload's `overlay_only` endpoint is reached from another node by its dependents and by no one
else, as on one node. What leaves a node into the overlay goes untranslated, since its source is a
workload address the peer allows.

**What crosses it is smaller.** WireGuard's encapsulation takes 80 bytes of an IPv6 packet's 1,500
(60 of IPv4's), so the interface's MTU is 1,420, which is `wg-quick`'s choice on such a link (§5 of
`research/evidence.md`). A workload's link stays at 1,500, so the node clamps the maximum segment
size of TCP connections into the overlay to fit, and answers a larger packet that may not be
fragmented with the ICMP message that says so, which a workload's table already accepts.

What the overlay does not do. It does not relay: two nodes neither of which is reachable from the
other share no dependency, and the scheduler says why. Edges are not members yet: §3.4 has an edge
tunnel to a node over WireGuard between Korpis machines, and how an edge joins this mesh is settled
with the edge tier (§3.1); until then nothing from the internet is routed into it. It does not hide
which nodes talk to which from someone watching the network between them. (Settled 2026-10-26 in
§3.1: an edge is a node and so a member, and what it forwards from the internet crosses the mesh in
a GRE link of its own pair, from each side's own mesh address, the last of its pool.) And it is
tested on the machine this project is built on with each node's data plane in a network namespace
of its own and a link between them, so that no interface is made in the host's namespace and no UDP
port is opened on its addresses; the paths between machines on separate networks, NAT among them,
are argued from WireGuard's documented behaviour rather than run there.

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
8. **Rules naming a domain.** A rule's peer may be a DNS name, and a packet filter sees addresses,
   and the addresses a name resolves to change under it, are shared between unrelated names behind
   a CDN, and differ between resolvers. A node could hold such a rule by learning the addresses
   from the answers its own resolver (§5.1) gives the workload and keeping each in a set for its
   record's lifetime, which is how Cilium enforces its `toFQDNs` policies; whether Korpis does
   that, and what a deny by name means for an address a second name shares, is undecided. Until
   then a declaration naming a domain is refused. → here
9. **Dependencies across projects.** A dependency opens the dependent's way to its target, and a
   target in another project, or another organization, is a workload the dependent's declarer may
   have no authority over. Whether that needs an action of its own on the target, granted by its
   owner, and how the target's owner sees and revokes it, is undecided; until then a dependency's
   target is in the dependent's project. → `08-identity.md`
10. **Re-verifying a domain.** A verified domain stays verified until its holder deletes it (§3.2),
    so a zone that changes hands leaves its old holder able to route the name here, though not to
    obtain a certificate for it. Checking the TXT record again on a schedule, and what happens to
    an endpoint whose domain fails the check, are undecided. → here
11. **Wildcard names.** An endpoint routing every name beneath a domain needs a wildcard
    certificate, which ACME issues only through the DNS challenge, and a route table where a
    wildcard and an exact name of different endpoints can both match one request. Until both are
    decided an endpoint names exact names. → here
12. **What the workload speaks.** The proxy forwards over HTTP/1.1, so a workload that serves only
    HTTP/2, which gRPC needs, cannot be routed. Whether an endpoint declares its backend protocol,
    and how that fits the frozen `EndpointProtocol`, is undecided. → here
13. **Rotating a node's overlay key.** A node's WireGuard key lasts as long as its identity (§9.1),
    so a key read off a node's disk stays good until the node is enrolled again. Whether the helper
    rotates it on a schedule, the agent reporting the new key and both keys being allowed while
    every peer learns it, is undecided. → here
14. **One address on several edges at once.** ECMP spreads one address's flows over every
    member that announces it and fails over in a rehash, faster than a lease's handoff (§5 of
    `18-operations.md`); §3.1 holds each address on one member at a time, fenced by an epoch,
    because a floating address on a shared link is answered by one machine, and two answering it
    is the split brain the lease prevents. Several members forwarding one address is safe where a
    router chooses among them, since every member forwards the same table; what it needs is a
    router contract naming several next hops, a liveness rule that withdraws a member rather than
    moving an address, and replies that find their edge after a rehash, which the mark per edge on
    the workload's node gives. Until then an ECMP or anycast router is told one holder. → here
