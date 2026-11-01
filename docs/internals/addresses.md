# Addresses

IPv6 is the default and IPv4 the scarce resource: how a workload's addresses are given, how inbound
is closed, address pools and dedicated addresses, and what a workload's network is on a node.

## Contents

- [IPv6 first](#ipv6-first)
- [Inbound is closed except where declared](#inbound-is-closed-except-where-declared)
- [Address pools](#address-pools)
- [Dedicated addresses](#dedicated-addresses)
- [Address routers](#address-routers)
- [A workload's network on a node](#a-workloads-network-on-a-node)
- [A guest's network](#a-guests-network)
- [The host's rules](#the-hosts-rules)
- [Other firewalls on the node](#other-firewalls-on-the-node)

## IPv6 first

Every workload gets a routable IPv6 address where its node has an IPv6 prefix routed to it. IPv4 is
allocated deliberately: shared by port (`node_port`, `stable`), shared by name (`ingress`), or
dedicated. IPv4 is the largest per-workload cost a hosting operator carries, so an operator serving
IPv6 clients natively and reserving IPv4 for those who need it has a materially cheaper cost
structure. Pelican added IPv6 allocations; treating IPv6 as the default rather than an option is the
next step.

## Inbound is closed except where declared

A workload's addresses, both families, accept connections only on the ports of its declared
endpoints, through a stateful firewall in its own network namespace; everything else is dropped. A
routable IPv6 address is an address, not an invitation.

The namespace's rule accepts a declared port from whoever sends to it, so the host also asks where a
connection comes from: a new connection forwarded to a workload's link from outside the node, from
neither another workload's link nor the mesh, is let in only where the node translated it from a
port of its own, or to a port of an endpoint exposed at the workload's own address (`node_port`,
`node_bound`, `dedicated_ip`). The rest is dropped and counted under the comment `exposure`. Without
it, an `overlay_only` endpoint on a node with a routed IPv6 prefix would be reached by the internet,
and an `ingress` endpoint directly, past the proxy's TLS, client certificates and limits. The node's
own processes reach every declared port, which is how the ingress proxy reaches its workloads; what
the mesh carries is judged by the mesh's chains. An `ingress` or `stable` endpoint is therefore not
reached at the workload's own IPv6 address; one that should be is declared `node_port`.

## Address pools

An `AddressPool` is a prefix an organization's network routes to its nodes, of `kind` IPv4 or IPv6,
`scope` node, edge or cluster, and `routing` static, BGP or a provider's API. `routing` decides
whether a dedicated address can move: static belongs to one node, and a workload holding one cannot
migrate; BGP or a provider API lets it follow the workload. A pool of scope `edge` names an [edge
group](edges.md).

**Whose pool.** An operator makes a pool for an organization with `address_pool.attest` on the root
organization, vouching that the organization holds the range, as `domain.attest` vouches for a name;
the organization deletes it with `address_pool.write`. Nothing proves who holds a range and no two
pools overlap, so if a tenant made its own, it could squat a range another organization holds and
leave it unable to make its pool. The attestation is the check, not a limit on size: Korpis gates
pools by neither size nor count. A pool serves its organization and those below, and its addresses
are held on that organization's own nodes alone, since a node holding an address translates every
connection to it and answers for it on its link: a pool on another's node would let its owner take
that node's traffic to any address it named, a public resolver's or the node's gateway, into its own
workload. A `node` pool names the node its addresses are routed to; a `cluster` pool names the
router that moves an address to whichever node holds it.

## Dedicated addresses

A workload with any endpoint exposed `dedicated_ip` holds one IPv4 address of its own, and every
such endpoint is reached there on its own port; what it sends beyond its node leaves from that
address too, which is what IP reputation means. IPv6 has no dedicated mode: every workload's IPv6
address is its own.

The control plane allocates the address in the transaction placing the workload, under the placement
lock, the lowest free one of a pool that serves it and reaches the chosen node. The placement filter
takes a node only where such a pool has a free address or the address the workload holds already
reaches it. Admission counts one `dedicated_ipv4` per workload, however many such endpoints it has.
Retired, the address returns to its pool after the pool's quarantine, a day by default, so what
still points at it, a client's cache or a name not yet changed, does not reach the next holder at
once.

**How a node holds it.** The helper translates every connection to the address into the workload's
own, all ports alike, in chains of priority -110, ahead of the node's forwarded ports (-100) and any
other table's destination translation, so the workload's namespace still accepts only declared ports
and the client is kept; and what the workload sends beyond the node, by any interface but another
workload's link and the mesh, leaves translated to the address, in the host's `postrouting` ahead of
its masquerade. A pool routed to the node, as hosting providers deliver an additional subnet, needs
nothing more; one on the node's link, answered by address resolution, names the interface, and the
helper adds the address as a `/32` host address without a route, recording in a rule's comment that
it added it, so an address the operator had put there is never removed.

**Where it may run.** A `node` pool's address is routed to that node alone, so its workload is
placed there only and a migration is refused, saying why. A `cluster` pool's address follows the
workload, moved by its router. A workload holding a `cluster` pool's address can also reach its
`stable` endpoints there where no edge group serves it, each on its own port.

The cost: an address idle for its quarantine after every retirement; a router, and a network or
provider that does what it asks, for an address that moves; a workload of a `node` pool that cannot
move. A workload holds one address, and the host's own processes never send from it.

## Address routers

A `cluster` pool names its router as a repository names its target: by the name of an
`address_router` provider its organization sees, its own before its ancestors', refused at creation
where none is seen. One call, `RouteAddress`, carries an address, the node to hold it or none, and
that node's advertised addresses as next hops, and asks the router to make the network route it so:
the whole of what is wanted, so a retry changes nothing. The router does what the network needs:
announces the address over BGP with the node as next hop, or moves a provider's floating address
through its API. BGP sessions and provider credentials are the router's, never the node's.

The control plane keeps, per address, the node the router last said it routes it to. A reconciler,
like the one publishing names, takes each address whose holder moved or that rests and is still
routed, holds it for a while so one control plane asks, and asks the router; a failure is retried
after ten seconds, doubling to ten minutes, and shown on the workload with the router and its reason
(`korpis status`). **What evaluates "two nodes never hold one address":** the control plane, which
writes the routed node only on the router's answer and gives the address to that node's assignment
alone; the router is asked only for the node of a committed placement, whose old node's assignment
has already lost the workload. At a migration's cutover the router is told the new node after the
lease's epoch changed. The node the address left answers for it until it applies its new set, one
assignment round. The router's view of the network is its own, so an address it says it routes and
does not is shown only as the workload unreached.

## A workload's network on a node

**The agent decides, the helper executes, the driver joins.** From the assignment the agent computes
the workload's addresses, endpoints, allocated ports and policy; the helper, which is root, makes
them so with `EnsureNetwork` and `ReleaseNetwork`, idempotent and keyed by workload; the driver
joins the namespaces in `/run/korpis/netns/<workload>` and declares `network_modes: veth` (or
`tap`), building no networking of its own.

**One namespace per workload, routed, no bridge.** The namespace has one interface, `eth0`, whose
peer on the host is a point-to-point link named for the workload (`kw` and thirteen more
characters). The host routes each workload's addresses to its link and nothing else; inside, the
default route is a link-local gateway, `169.254.1.1` and `fe80::1`, answered by the host side. With
no shared layer 2, no workload can answer another's ARP or neighbour discovery, flood a bridge, or
see a frame not addressed to it, and every packet crosses the host's routing and nftables, where
[policy](network-policy.md) applies. The cost is a route and firewall lookup per packet that a
bridge would skip between two workloads on one node, traffic the default policy denies anyway.

**Addresses come from the node's pools, allocated by the agent.** IPv4 comes from a pool private to
the node: the /22 the control plane gave it at enrollment, so the [mesh](mesh.md) can route between
nodes, or one the operator names with `--workload-ipv4`, since a pool colliding with the operator's
network breaks it and only the operator knows that network. IPv6 comes from a prefix the operator
routes to the node (`--workload-ipv6`); a node given none uses a unique local prefix derived from
its identity, translated on the way out. What leaves by any other interface is translated to the
node's address: all of IPv4, and IPv6 from a unique local address. The agent allocates the lowest
free address of each pool when a workload first runs on the node, keeps it on disk, releases it at
retirement, and reports it in the observation (`Observation.address`), since an address is a fact
about a node.

**Inbound is closed in the namespace itself.** Its own nftables table drops input by default,
accepting loopback, replies, the ICMP and neighbour discovery a network needs, and new connections
only to declared ports over their protocols, from anywhere, the node and its other workloads
included.

**The namespace is owned by the workload's own user namespace**, because the kernel lets a process
mount sysfs only in a network namespace its user namespace owns, and a container's root is root only
in its own. The helper makes the pair, mapping only the range its ledger records for the workload,
so no request makes it map the host's ids. Owning the namespace does not let the workload change it:
it holds no `NET_ADMIN`, and its seccomp profile refuses making a namespace in which it could. A
kernel flaw letting a process raise its capabilities in its own user namespace would let it rewrite
its own addresses and firewall, which a host-owned namespace would have held, and the host's rules
are what keep a rewritten address from reaching anything. A user namespace can only be made by a
single-threaded process, so the helper makes the pair in a short-lived child and never starts a
second thread itself.

## A guest's network

A guest's interface is its own kernel's, so its addresses are set inside it from its config drive,
and the namespace keeps the link. The helper makes a persistent `tap0` beside `eth0`, owned by the
workload's first id, which the jailer runs Firecracker as, so no other host user can attach to it.
The namespace routes between the two as the host routes between a link and its uplinks: `eth0` holds
no address of the workload's, which are routed to the tap, and the default route is the gateway on
`eth0`. It answers for each side on the other by proxy ARP and proxy NDP, so the host sees the guest
where it saw a container and the guest sees `169.254.1.1` and `fe80::1` as a container does. The
namespace reaches the gateway through a permanent neighbour entry for the host side's hardware
address, which the helper sets from the hash the link's name is made from. Everything on the host
side is unchanged.

The namespace's firewall moves to its forward hook, and its input admits from the tap only queries
to the resolver. A guest is root in its own kernel and can send from any address, which a container
without `NET_ADMIN` or `NET_RAW` cannot, so what comes from the tap is taken only from the
workload's own addresses and link-local ones, dropped and counted otherwise, before any other rule,
with a strict reverse-path filter besides; otherwise a guest could pass for another workload. The
namespace's own forwarding, proxy and reverse-path settings are sysctls of that namespace alone,
never the host's. A namespace made for one interface kind is made again for the other.

## The host's rules

The helper keeps the host's rules in one nftables table, `inet korpis`, with a chain per workload
reached from verdict maps keyed by its interface, and replaces a workload's chains and entries in
one transaction, so a change is never half applied. It writes them with `nft`'s JSON input, the
interface nftables documents as stable, rather than through a C binding, which keeps nftables' C
library out of the helper; links, addresses and routes are made over netlink. A node needs nftables
1.0 or later. A pool colliding with the operator's network is the operator's to change, which
renumbers workloads at their next start.

## Other firewalls on the node

A node whose operator runs another firewall with a forward chain dropping what it does not accept,
which Docker and ufw both install, drops every workload's traffic beyond the node, because nftables
lets every base chain on a hook drop a packet whatever another accepted. The helper never edits
another firewall: it reports such chains, and the agent logs them at start with the rule opening
each for Korpis's links. Until the operator opens them, workloads reach the node and the node
reaches them, and nothing else does. Likewise a node with IPv6 forwarding off gives its workloads
IPv4 alone and says so; the helper does not turn it on, since that stops the node taking router
advertisements on its own interfaces.
