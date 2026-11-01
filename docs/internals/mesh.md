# Mesh

How nodes reach each other, how a node's reachability is measured, and what changes for each
exposure mode when a workload moves. Cross-node dependencies, edges and volume transfers all cross
the mesh.

## Contents

- [Optional, with an explicit requirement](#optional-with-an-explicit-requirement)
- [Reachability is measured](#reachability-is-measured)
- [Keys and interfaces](#keys-and-interfaces)
- [Every node's pool is its own](#every-nodes-pool-is-its-own)
- [Peers](#peers)
- [Both ends hold the policy](#both-ends-hold-the-policy)
- [Volume transfers](#volume-transfers)
- [What changes at cutover](#what-changes-at-cutover)
- [What it does not do](#what-it-does-not-do)

## Optional, with an explicit requirement

The mesh is WireGuard: in the kernel, encrypted, low overhead, and tolerant of nodes on ordinary
internet connections, which matters because a node dials out to the control plane and may be behind
NAT. That is also its limit: a node needs no inbound port from the control plane, but a mesh needs
nodes to reach each other, and a node behind symmetric NAT reaches the control plane and not its
peers. NAT traversal by hole punching works often and fails unpredictably, which is worse than
absent; relaying everything through the control plane makes it a data-path bottleneck and breaks the
data plane's survival of its loss. So the mesh is optional and its requirement explicit: nodes that
can reach each other join it, and a node that cannot is `overlay: unavailable`, and the scheduler
places no workload with a dependency on another node there.

An `overlay_only` endpoint does not itself need the mesh: it is reachable on its own node through
the node's network, and across nodes only through the mesh, so what needs the mesh is a consumer on
another node, which is a cross-node dependency. Placed without the mesh, such an endpoint is
reachable from its own node only, and its plan says so.

## Reachability is measured

A node cannot know from inside whether it is reachable, so the control plane measures it
(`Node.reachability`), and every exposure mode states what it needs:

| Exposure | Requires of the node |
|---|---|
| `node_port`, `node_bound` | inbound reachability on its ports |
| `stable`, `ingress` | a tunnel to its edge group, which the node dials, so NAT does not matter; without a group, what `node_bound` needs |
| `overlay_only` | nothing for consumers on its node |
| a dependency on another node | the mesh |

So a node behind CGNAT hosts game servers through an edge, and cannot on its own address. Unmeasured
is treated as unreachable (`edge_only`), since the opposite would place a workload on an address
nobody may reach and report it served.

**What is measured.** A node advertises, in its Hello and every heartbeat, the addresses clients
reach it at and its port range. The addresses are its operator's (`--advertise-address`), or else
the address its traffic to the control plane leaves from, only if routable on the internet; a node
behind NAT advertises nothing of its own accord, which is the truth about it. The range is
`--ports`, 30000 to 32767 by default, below Linux's ephemeral range so an allocated port never
collides with an outgoing connection's, and never below 1024. A change of either makes the node
unmeasured again; a changed address is noticed at the next heartbeat.

**The probe.** The replica holding a node's session measures it when unmeasured against what it
advertises, and every ten minutes. It asks the node to listen for fifteen seconds on a free port of
its range that no endpoint holds (the node refuses one it forwards), over TCP and UDP on every
address; the node answers each 32-byte nonce with its node key's signature over a message bound to
the purpose, node and port, so a machine holding the address but not the key is not taken for the
node. A node is `public` if one address answers over both TCP and UDP, since a game server's port is
usually UDP and a web endpoint's TCP and a firewall passing one need not pass the other; a node
passing only TCP is therefore `edge_only`. A port the node cannot listen on says nothing of
reachability, so the prober tries the next free one, up to three. A measurement is recorded only if
the node still advertises what was measured.

The probe measures from where the control plane runs. Inside the node's network it reaches a private
address no outside client can, and on the node's own machine, every single-machine installation, it
reaches the public address without crossing a firewall in front of the machine, such as a cloud
security group. So on one machine `public` means the node answered from inside, and the plan
exposing an endpoint says the outside firewall is the operator's.

## Keys and interfaces

Each node has one WireGuard interface, `korpis-overlay`, made over route netlink and configured over
WireGuard's generic netlink family, listening on UDP 51820 unless `--overlay-port` says otherwise.
The helper generates its private key into its own state the first time (`overlay.key`) and sets it
only on a link that has none, so a link kept across a helper restart keeps its key; it never leaves
the node. The agent reports the public key and port in its Hello and heartbeats, so the control
plane holds each key as the node's authenticated session said it. A compromised control plane can
name any key as a node's peer, which gives it nothing it lacks. The key lasts as long as the node's
identity, replaced only by enrolling again; rotation is not built. The interface's MTU is 1,420,
since WireGuard takes 80 bytes of an IPv6 packet, so the node clamps TCP's segment size into the
mesh and answers an oversized unfragmentable packet with the ICMP message saying so.

## Every node's pool is its own

Cross-node traffic is routed, not translated, so a workload sees its client's own address, which
needs pools no two nodes share. The control plane, which alone sees every node, gives each node a
/22 of an installation range (`10.48.0.0/12` unless `--overlay-range`) at enrollment, held until it
is decommissioned; the agent keeps it in its state beside its identity, since it lays out networks
at start before any session. A /22 is 1,022 workloads; the default range holds 1,024 nodes. A range
with no free /22 enrolls a node without one, its enrollment effect saying so. A node's IPv6 is its
unique local prefix derived from its identity.

**A member's pools are what the control plane gave or can derive**, never what the agent reports:
every member routes a member's pools through the mesh, so a compromised node reporting the
operator's LAN or half the address space would have every node route it, hosts' own traffic
included, to itself. They are the IPv4 pool given at enrollment, `10.47.0.0/16` for a node enrolled
before pools were given, and the unique local IPv6 prefix, which the control plane computes as the
agent does. A node reporting another pool, including one its operator named with `--workload-ipv4`,
or overlapping an earlier member's, is outside, `overlay_outside` saying why without naming the
other node, whose organization may be another; enrolled again it gets a pool of its own. An
operator's routed IPv6 prefix is left to the operator's network and not carried.

## Peers

Every enrolled node is a member, whatever its organization, since membership reaches no workload by
itself. Two members are paired only where they may host workloads of one project: one's organization
is the other's or above it, so a tenant's node is paired with the installation's and never with a
sibling tenant's, and `Node.peers` lists only nodes the caller may read. For each paired member a
node is sent its public key, an endpoint (its first advertised address and port, or none) and its
pools as allowed IPs. WireGuard routes a packet to the peer whose allowed IPs hold its destination
and accepts a decrypted packet only if its source is in the allowed IPs of the peer whose key
decrypted it, so a packet arriving from a workload address is authentically from the node holding
that pool. The agent writes peers and routes and keeps them on disk (`overlay-peers.binpb`), so the
mesh runs with the control plane gone; peers are updated in place and only those no longer named
removed, since replacing them wholesale ended every session on each pass. Each replica computes the
mesh every two seconds and each session sends its node what changed.

**A pair is reachable when both nodes report a fresh handshake.** A node behind NAT has no endpoint;
it reaches a peer that has one, keeping the mapping open with a keepalive every 25 seconds, and the
peer learns its endpoint from authenticated packets. So a pair works when at least one node is
reachable, and two unreachable nodes cannot pair, with no relay. A session older than 180 seconds
carries no data (WireGuard's Reject-After-Time) and one in use is renewed every 120, so a pair is
reachable when both nodes' last reports name a handshake with the other younger than 180 seconds.
`Node.overlay` is `available` when the node reaches every member it is paired with. A pair is judged
at most a heartbeat late.

## Both ends hold the policy

The source drops early and the destination does not trust the source. The dependent's node opens a
remote target's address and port in the dependent's chain only out through `korpis-overlay`,
untranslated. The target's node does not take that on trust, since a compromised node could send
anything: the control plane, which resolved the dependency, gives it each remote dependent's address
and the endpoint it may reach, and it accepts from the mesh only replies and those, counted under
`dependency`, dropping the rest, to its workloads and itself alike, under `overlay`. Nothing else
crosses into a workload, so an `overlay_only` endpoint is reached from another node by its
dependents alone. A change to a workload wakes the nodes of its dependents and targets.

## Volume transfers

A replicated volume's increments and a migration's copies are sent through the mesh, never through a
third node. The receiving agent listens on TCP 51820 of its own mesh address, the last of its IPv4
pool, which the overlay puts on the interface; the helper opens the listening socket and the sending
socket bound to that address in the namespace it keeps the mesh in, and hands them to the agent. The
node's table admits that port from the mesh only for the mesh addresses of nodes its `Replicas`
message names as senders, under `transfer`, and drops what reaches the port by any other interface,
since Linux takes a packet for any of its addresses on whichever interface it arrives; the agent
reads nothing from a peer it was not told sends it a replica. See [replication](replication.md). The
host's table is written whole at each pass, every ten seconds and on change, so its counts are since
the last pass: a reading for an operator looking now, not a total.

## What changes at cutover

| Exposure | At cutover |
|---|---|
| `node_port` | the address changes, and clients must be told: why it is not the default |
| `node_bound` | the original node's forwarding target is updated while that node exists |
| `stable` | the edge group's forwarding target is updated |
| `ingress` | the proxy's upstream is updated; the name and certificate do not change |
| `dedicated_ip` | a cluster pool's router moves the address; a node pool's refuses the migration when proposed |
| `overlay_only` | mesh routes update and dependents are re-injected |

None is a step of the migration's own: each is kept by the reconciler that already keeps it from the
workload's authoritative placement, and the placement moves in the one transaction `cutover` makes,
which waits for the source to release its epoch or for that epoch's term to end. So each follows a
move by the path it follows a failover.

**The guarantee is single ownership at every instant, and continuity is a separate, shorter
promise.** A BGP re-advertisement converges over seconds, a provider API moves an address in its own
time, an edge group is several machines updated one by one, and existing conntrack entries keep a
flow on its old target until they expire. What holds: no node that has lost the lease serves
traffic, since the source stops under its epoch before the destination's is granted, so a packet
still reaching the source finds nothing listening and is dropped, and misdelivery to a second copy
is impossible while loss for a moment is not; forwarders flush the moved address's connection
tracking when they switch, so UDP flows do not linger; and the plan states the window per mode:
sub-second for an edge group, the configured convergence for BGP, the provider's documented time for
an API move.

## What it does not do

It does not relay: two nodes neither of which reaches the other share no dependency, and the
scheduler says why. It does not hide which nodes talk from someone watching the network between
them. It is tested on the project's build machine with each node's data plane in a network namespace
of its own, so paths between machines on separate networks, NAT among them, are argued from
WireGuard's documented behaviour rather than run.
