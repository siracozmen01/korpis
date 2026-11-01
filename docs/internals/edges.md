# Edges

An edge owns stable public addresses and forwards to wherever a workload currently is, so a game
server moves between machines, and survives the loss of one, without a player editing a server list.
That makes migration ordinary rather than an event, and draining a node routine. The exposure modes
are on [endpoints](endpoints.md).

## Contents

- [Groups](#groups)
- [Addresses and ports](#addresses-and-ports)
- [Forwarding and the tunnel](#forwarding-and-the-tunnel)
- [The lease](#the-lease)
- [Liveness through the forwarding path](#liveness-through-the-forwarding-path)
- [Failover](#failover)
- [When nothing is behind an address](#when-nothing-is-behind-an-address)
- [Where a workload may run](#where-a-workload-may-run)
- [Ingress through a group](#ingress-through-a-group)
- [What it costs](#what-it-costs)

## Groups

`stable` needs edges in failover groups of at least two, or a pool of addresses routable to any
node; an address that does not survive the loss of the machine holding it is `node_bound`, whatever
it is called, so a group with fewer than two members serves nothing. An edge is a role a node holds,
not separate hardware.

An edge group belongs to an organization and names two or more of that organization's own nodes as
members: an edge translates every connection to the addresses it holds, so a group on another's node
would take that node's traffic. Its addresses are address pools of scope `edge` naming the group,
made by an operator's attestation as every pool is ([addresses](addresses.md)), IPv4, IPv6 or both.
A group serves the workloads of its organization and those below, the nearest group first, as a
provider's name is resolved. Korpis limits neither members nor addresses. Groups are managed with
`EdgeGroupService` and `korpis edge-group`.

## Addresses and ports

The control plane allocates a `stable` endpoint's address and port in the transaction placing the
workload, once the endpoint resolves to `stable`, and keeps them while the workload declares the
endpoint, across every move and stop. Addresses are shared by port, which is what stretches IPv4: a
workload's `stable` endpoints all get one address, the lowest of the pool on which each can have its
declared container port, or else a port of the group's range (1024 to 65534 by default), so a game
server's players keep typing 25565. A port is held per transport, since the edge's table is keyed by
address, transport and port, so a game's TCP port and its query's UDP port share a number as
Minecraft's do. With an IPv6 pool too, each endpoint gets the same port at an IPv6 address.
Released, an address and port rest for the pool's quarantine before another workload is given them.
The plan says the address is not the node's.

## Forwarding and the tunnel

**Every member forwards the whole group; one holds each address.** Each member is told the group's
table beside its assignments and keeps it on disk: for every allocated address, transport and port,
the node its workload is placed on, the workload's address and its container port, or no target when
it is not running. A member taking over an address has its forwarding in place, and only the address
moves. The table is recomputed every two seconds and sent when it changes.

How the network learns the holder depends on the pool. A `static` pool is on a link the members
share: the holder adds the address to the pool's interface as a host address and announces it with
gratuitous ARP for IPv4 and an unsolicited neighbour advertisement, override flag set, for IPv6,
which is VRRP's floating address without VRRP's election, since the lease decides; an address the
interface already had is the operator's and never taken off. A `bgp` or `provider_api` pool names an
address router, which the control plane asks to route the address to the holder; the holder adds it
to its loopback, announcing nothing, so a port no endpoint holds is refused there rather than routed
back out. The router is told the member its lease was last issued to while that member has not
released it and a `stable` port holds the address, and the group's status shows where the router
last routed each address and why not to its holder. The holder holds the address from its grant,
before the router has routed it, since the lease and not the route keeps two members from holding
it.

**The tunnel keeps the client's address.** Edges are members of the [mesh](mesh.md), so a node
behind NAT dials the edges it is paired with and `stable` needs only an outbound tunnel of a node.
The holder translates a new connection's destination into the workload's address and port and
carries it to the workload's node over GRE inside the mesh: one GRE link per pair of edge and node,
between each side's mesh address, the last of its pool, which no workload is given and which the
helper puts on `korpis-overlay`. GRE carries IPv4 and IPv6 in one link. The workload's node takes
from an edge's link only connections to the ports its group forwards to that node's workloads, marks
each with the edge it came through, and routes replies back into that link by the mark; the edge
reverses the translation and replies from the stable address. Each packet through a link carries its
mark before routing, so the kernel's reverse-path check finds the way back, and the host's own
setting, strict or loose, is left alone and passes. GRE to a node's mesh address arriving other than
through the mesh is dropped before the kernel takes it, since the kernel gives a GRE packet to the
link its outer addresses name whatever interface brought it. A node's own mesh addresses are routed,
where its main table has none, by a table of Korpis's own whose default is the mesh, because the
kernel restores a packet's mark only once a first route is found. An edge's link is judged by the
edges' chains alone, not the workload's exposure chain.

IPv6 stable addresses go to the workload's IPv6 address as its node observes it, refused where it
has none, through the same GRE link, IPv4 outside. A host whose firewall checks IPv6 sources by a
routing lookup before Korpis marks the packet refuses what arrives, since IPv6 has no
`src_valid_mark` and the helper leaves the host's firewall alone; a node with IPv6 forwarding off
forwards no IPv6 address.

## The lease

Every other authority in Korpis is fenced, and an edge forwarding for hundreds of workloads while
holding nothing would allow the one failure the system exists to prevent: traffic not passing on a
host that is up, every agent healthy, every lease valid, every observation correct, and the service
down. So an address held by an edge is fenced as a workload is ([node agent](node-agent.md)): the
member acquires and renews it by request, its term running from the send on the holder's side and
from receipt on the control plane's, and a new epoch for another member is issued only once the
holder said it released the address, having removed it and stopped forwarding, or the control
plane's end of the term has passed. The term is the group's, ten seconds by default and never under
three, shorter than a workload's since a held address answering nothing is an outage. **What
evaluates "two edges never hold one address":** the control plane, writing holder and epoch under
the group's row lock and granting only by that rule; and the member, which stops holding at its own
end of the term, before the control plane's. A member renews a third of the way through its term,
and a holder no longer named stops renewing and so lets go. An agent that restarts gives up its
addresses until its leases are granted again, a round trip.

The control plane names in each member's table the addresses it should hold: a ready holder keeps
its address; otherwise a ready member takes it, each address going to the member a hash of address
and member weighs heaviest, so addresses spread and a member joining or leaving moves only its own.
Where no member is ready, the control plane suspects its own probe and judges by sessions alone: a
holder still connected keeps its address, so a control plane that reaches no member through its
probe still has every address held.

## Liveness through the forwarding path

Liveness is measured through the path a player takes, not from the process that owns it: only that
tells "the edge process runs" from "the edge forwards". The edge cannot measure itself; a peer
sending through the holder sees its answer delivered locally; and the workload's agent knows nothing
of the edge. So the control plane probes, from outside every node, as a client does.

One replica at a time holds each group's probe, for ten seconds renewed every two. Every member
forwards a probe port of the group's (65535 by default, outside its range), on its own first
advertised address of each family and on each address it holds, over UDP, to a node the group
forwards to, through the translation and GRE link a player's packet takes. That node's agent answers
a 32-byte nonce with its node key's signature over a message bound to the purpose, group, address
and node, as a [reachability probe](placement.md) is answered; so an answer proves the packet
crossed the member's translation and tunnel and came back through them. The control plane sends one
probe a second to each, and a member missing three in a row is failing: the declared detection
interval, three seconds unless the group sets either figure, shown on the group. One answer makes it
ready again.

A control plane on the same machine as the node it probes through sees its reply delivered locally,
measuring only the way in. So every twenty seconds each replica tries to bind to each mesh member's
own mesh addresses, which succeeds only in its own network, and the tables name as the probe's
target a node no such record names where there is one. A host letting a process bind addresses it
does not hold finds every node its own; an installation of one node has no other to choose.

## Failover

When the holder is failing, its session has ended, or it is not ready, and another member is, the
control plane stops naming the holder and grants the next ready member the address at the next epoch
once the lease rule allows, then asks the pool's router where there is one. A connected holder
hearing the control plane releases at once, so failover takes the detection interval and a round
trip; one unheard, the rest of its term. Each member flushes the connection-tracking entries of an
address it stops holding and of a port whose target moved, so a UDP flow follows its port at its
next datagram. The group's status names each address's holder, epoch and whether it answers through
its holder, and each member's readiness, last probe and why it is not ready (`korpis edge-group
get`). TCP clients see a reset and UDP clients a timeout; both reconnect to the same address.

## When nothing is behind an address

The address outlives the placement, so an edge routinely holds an address whose workload is stopped,
hibernated, unschedulable, or on a dead node. A black hole costs the person connecting thirty
seconds and a timeout they blame on their own connection; a refusal is immediate and honest. So a
forwarded port whose workload is not running, and every port of a held address no endpoint holds, is
refused at once with a TCP reset or an ICMP port unreachable, never a black hole. A neighbour's
solicitation of a held IPv6 address is answered, since refusing it would end the neighbour's sending
to it at all. Where a protocol router is registered it can answer with the real state (`hibernated`,
`starting`, `unreachable`), the affordance that holds a player's client while a hibernated server
boots.

## Where a workload may run

The placement filter asks of a candidate node that it reaches every member of the group through the
mesh. A workload whose group has no ready member, or whose address has no holder or is not yet
routed to its holder, stays running and placed where it is, its `stable` endpoints shown unreached
with the group and reason named, which the control plane knows from its own probes. It is not
`unsatisfiable`, which its node cannot observe and which would move it to no avail.

## Ingress through a group

An `ingress` endpoint a group serves is reached at the group's ingress address: one address of its
pool kept for ports 80 and 443 over TCP and UDP, chosen at the first placement needing one as the
lowest of the group's first pool of that family whose ports 80 and 443 no `stable` endpoint holds,
and kept for as long as its pool is, so names resolving to it do not move. Every member runs the
[ingress](ingress.md) proxy with the routes and certificates of every name the group serves; the
holder translates the address's ports to its own proxy, so the proxy sees the client, and the proxy
reaches each workload through the mesh as a dependent does, over TCP, the workload's node admitting
the member's mesh address to the endpoint's port. The ingress address's lease is granted only to
members serving the ingress.

Which group serves a workload's `ingress` endpoints is decided where it is placed: the group serving
its `stable` endpoints where it can, else the one serving its ingress already, else the nearest
organization's group that can, else the node's own proxy. A group can where two or more members
serve the ingress and it has an IPv4 ingress address or room for one. A node is asked to serve the
ingress itself unless a group serves the `ingress` endpoints. A certificate for those names is given
to every member, and an order's challenges reach every member; the issuer tells the directory to
look once every connected member serving the ingress said its proxy holds them, since whichever
holds the address when the directory looks must answer.

## What it costs

Two or more machines doing an edge's work, whose addresses reach every node behind them; a GRE link
per pair; 24 bytes of each packet, so TCP's segment size is clamped to the link's MTU, 1,396; a
conntrack entry per flow on the holder; a probe a second per member; and, for a holder that dies
unheard, up to one term of nothing. Through a group, a member's proxy routes every name its group
serves, and a workload is one hop further than through its own node's proxy. An edge does not hide
traffic from its members, which see it as any router does, so a group is its organization's own
nodes; a compromised member can drop what it forwards, which the probe sees only if it drops the
probe too. One holder per address is the design: spreading one address over several members at once,
as ECMP would, is not built, and a router is told one holder, never every member. Direct server
return is not built, so replies cross the edge.
