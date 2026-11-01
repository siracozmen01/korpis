# Endpoints

How a workload is reached: the endpoint, its exposure modes, ports on a node, what the workload sees
of its client, and the DNS records Korpis publishes. Edges are on [edges](edges.md), name-based
routing on [ingress](ingress.md), addresses and a workload's network on [addresses](addresses.md).

## Contents

- [The address must not be the machine](#the-address-must-not-be-the-machine)
- [The endpoint](#the-endpoint)
- [Exposure modes](#exposure-modes)
- [Ports on a node](#ports-on-a-node)
- [Who the workload sees connecting](#who-the-workload-sees-connecting)
- [Protocol-aware routers](#protocol-aware-routers)
- [DNS records](#dns-records)

## The address must not be the machine

Pelican fixed nearly every surface complaint about Pterodactyl, and an allocation system rework is
still on its planned list. The reason is structural: Pterodactyl's `Allocation` is a bare `(ip,
port)` row belonging to a node, and the server's address, the firewall, what the customer sees and
what the billing system provisions all hang off it. Replacing it means replacing the schema, so
nobody who inherited it can. The category therefore lacks ingress, automatic TLS, egress control,
traffic accounting, bandwidth limits (pterodactyl/panel#1871, #3352), usage visibility (#3780) and
service discovery.

And `(ip, port)` on a node ties a customer's address to a machine: migrating a server changes its
address, every player's saved entry breaks, and every DNS record, firewall rule and billing record
that named it is wrong. That is why migration is dreaded even where it works. Decoupling the address
from the placement is the central goal of Korpis's networking.

## The endpoint

```
Endpoint
  id, workload, name       "game", "rcon", "query", "http"
  protocol                 tcp | udp | http | https | quic
  container_port
  exposure                 declared, and resolved per placement
  address                  assigned; its shape depends on the exposure
  client_address           preserved | translated | proxy_protocol
  domains, tls             for ingress
  policy                   who may connect, and how many
  accounting, shaping
```

One object covers a Minecraft UDP port, an RCON TCP port and an HTTPS ingress with a certificate;
they differ in field values, not in kind. An endpoint keeps its identity while its workload declares
it, moving with the workload, and a workload's status lists them, each with where a client reaches
it.

## Exposure modes

| Mode | Address | Survives | Costs | Right for |
|---|---|---|---|---|
| `node_port` | the node's address and an allocated port | no move | nothing | hobby, development |
| `node_bound` | an address on the node where it was first exposed, forwarded to wherever it runs | a move while that node exists | one hop after a move | single machines, fleets without edges |
| `stable` | an address held by an edge group, forwarded to the current placement | a move and the loss of any one node or edge | an edge group, or a routed pool | game servers in production |
| `ingress` | name-based, shared address | a move | a proxy | HTTP, HTTPS, TLS services |
| `dedicated_ip` | an IPv4 address of the workload's own | a move, if the address is routable to any node | an address, and a router | games needing a whole address, IP reputation |
| `overlay_only` | reachable by other workloads only | a move | the mesh | databases, internal services |
| `none` | none | | nothing | bots, batch jobs |

`node_port` is Pterodactyl's behaviour, kept because on a single node it is correct and free, and
offered deliberately rather than inherited; it is not the default, and the panel says the address
changes if the workload moves.

**The default is `stable` where the installation can deliver it, and `node_bound` where it cannot.**
A recipe's `exposure_default: stable` is a preference, not a promise. The intent keeps the declared
mode, so an installation that gains an edge group honours it; the scheduler resolves it per
placement, knowing whether a group serves the workload, and judges reachability on the resolved
mode, so a `stable` endpoint without a group needs what `node_bound` needs: a node reachable on its
own address. `stable` resolves to `stable` where an edge group of two or more members serves the
workload and has an address and port for each of its `stable` endpoints; else, where a `cluster`
pool serving it has a free address, to that address, held as a `dedicated_ip` workload's is, each
endpoint on its own port; else to `node_bound`. The endpoint records the resolved mode.

`node_bound` and `stable` were once one mode whose default edge sat on the workload's node and
forwarded to a migration's destination for a drain interval, after which the address belonged to a
node the workload had left, so the promise that no player edits a server list failed one interval
after every move. Removing the hop and keeping the guarantee are not both available, so each is a
named mode: `node_bound` adds no hop while the workload is where it started, forwarding being a
kernel table lookup on that node, and one hop after a move, for as long as it stays away; a drain
decommissioning its node lists in its plan every `node_bound` address that will change, offering
`stable` first where possible. `stable` costs one hop through the edge inbound, and back unless the
node permits direct server return. This market's users feel five milliseconds, so the hop is shown
where the mode is chosen.

**What the plan shows.** The plan that creates an endpoint, or changes its declared exposure,
carries for each the mode declared, the mode it resolves to in this installation, and what that
survives: no move for `node_port`, a move while its node exists for `node_bound`, a move and a
node's loss for the modes an edge, a proxy, a routed address or the mesh holds, nothing for `none`.
For an endpoint on a port of its node it says too that the node's reachability was measured by the
control plane, which on one machine is inside the node's network, so a firewall in front of the node
is the operator's to open. It is computed from the declared endpoints and whether a group serves the
workload, both of which the control plane holds; the node is not yet known, so it says what any node
of the installation gives.

## Ports on a node

**The control plane allocates each `node_port` and `node_bound` endpoint a port** from the range its
node advertises, when it computes the node's assignment, since it alone holds every placement on the
node and so can keep two endpoints off one port. The port stays the endpoint's while the workload is
placed there, across a change of the node's range, so a narrowed range does not move an address a
player has; new allocations come from the range as it is. One node's ports are allocated once
whatever the transport. On one node `node_bound` behaves as `node_port`; what distinguishes them is
a move.

**A node's ports are a placement dimension.** Allocation runs after the bind, when the only choices
left are failing the node's whole assignment set or leaving the endpoint unreached, so `node_ports`
is a reserved dimension like memory: a node's allocatable is its range less one port kept free for
the reachability probe, and a placement reserves one per endpoint resolving to `node_port` or
`node_bound`. It is a quota dimension too, since ports are as scarce as addresses. An endpoint
resolved to `stable` holds its edge group's address and port and none of its node's; a quota
bounding `node_ports` still counts it, since admission precedes the placement that resolves it. A
range narrowed below what its endpoints hold leaves an endpoint without a port, and its status says
so.

**How the node forwards.** The host's table has one map from a port of the node to the chain of the
workload holding it, consulted for every new connection to an address of the node, from outside or
from the node itself; each workload's chain translates its ports to its own address and container
port, by destination translation alone, so the workload sees the client's address. A workload's
ports change in one transaction with its chain, so a port is never forwarded to a chain being
rewritten. The workload's own table accepts new connections only to its declared ports, so a port it
listens on without declaring is closed to everyone, the node included. The node reports each
endpoint at the address it gave the workload. Loopback is not forwarded, since a packet from
127.0.0.1 cannot leave the host for a workload's link, so a client on the node reaches an endpoint
at another of its addresses. A node changing its advertised addresses changes every such endpoint's
address, and each flow costs a conntrack entry.

## Who the workload sees connecting

Game servers ban, rate-limit and show moderators by address, and the obvious implementation, DNAT in
and SNAT back, replaces every player's address with the edge's.

**Edges and forwarding nodes tunnel; they do not translate the source.** The inbound packet is
carried to the workload's node in GRE inside the mesh, the outer header from the edge's own address,
which WireGuard accepts as the edge's, while the inner one keeps the client's, which WireGuard alone
would refuse as outside the peer's allowed addresses. The reply follows policy routing back into the
tunnel, and the edge sends it from the stable address. So the workload sees the real client for TCP
and UDP alike with no cooperation from the application (`client_address: preserved`). Where a node's
upstream permits sending with the edge's address as source, replies could leave directly (direct
server return); many networks rightly filter that, so it would be detected per node and never
assumed, and it is not built.

**Userspace routers translate, and say so.** The HTTP ingress and a protocol-aware router terminate
the connection, so the workload's peer is the router. They pass the client in the `Forwarded` header
or the PROXY protocol version 2, and where the workload does not take it, the endpoint's
`client_address` is `translated` and the interface says the workload sees the router, not the
player.

## Protocol-aware routers

Some protocols name a host in their handshake: Minecraft Java sends the address it was asked for,
and supports DNS `SRV`, so a router reading it could name-route many servers behind one address and
port. That router is an [extension](extensions.md), not core, since no game appears in the core and
a Minecraft handshake parser in the ingress path would be one. Core provides the forwarding plane
and a hook for a router to register as a port's handler. UDP carries no name, which is why most game
traffic cannot be name-routed generically, and why `stable` exists: the address is the identity.

## DNS records

**Korpis sends an owner's records whole, and the provider makes its zones agree.** An owner is what
records exist for: an endpoint and one of its names (`ep_.../<digest>`), or one name's ACME
challenge of a certificate (`crt_.../<n>`). `ApplyRecords` carries every record that owner should
have (`A`, `AAAA`, `CNAME`, `SRV`, `TXT`; a lowercase name, a TTL of 30 to 86,400 seconds, values;
at most a hundred), and the provider creates what is missing, changes what differs, and removes what
the owner had and no longer has. Whole sets make the call idempotent, so a retry after a lost
answer, a provider replaced midway, or a resync after an outage all converge, and the control
plane's reconciliation from desired state needs nothing else. The provider never touches a record it
does not own, so it keeps an ownership registry (a TXT marker beside each, as external-dns does, or
its own tags), which is the price of never deleting a record a person made. A name in no zone it
manages fails `dns.zone_not_found`, one held by a record it does not own `dns.conflict`, neither
retryable. The control plane checks every record before any provider is called.

**What is published.** Each name an ingress endpoint routes resolves to its node's advertised
addresses, or its edge group's ingress addresses, an `A` and an `AAAA` set with a minute's TTL, so a
moved workload is found a minute after its records change. Only names an ingress endpoint routes are
published, and a name is routed only by the organization holding it ([ingress](ingress.md)). Each is
published through whichever provider serves its zone: the one that published it before, then the
others the organization sees, its own before its ancestors'. **What evaluates whether a provider
serves a zone:** the provider, from its zones; the control plane holds no list of zones and learns
by asking, at the cost of one refused call per provider ahead of the one that serves. A name no
provider serves is the holder's to publish, shown `unmanaged` in the workload's status, and
providers are asked again hourly. A name no longer routed is withdrawn. A change of an endpoint or
its node's addresses wakes the reconciler, which also looks every thirty seconds and takes each name
under a lease, so replicas share the work. A failure leaves the workload running: the name is
`failed` in its status with the provider and reason, retried after five minutes, doubling to a day.
Nothing is published for an endpoint of any other exposure, which names no domain.
