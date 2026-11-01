# Bandwidth

How a workload's traffic is counted and shaped, and what Korpis does about attacks. No usage
visibility (pterodactyl/panel#3780) and no bandwidth limit (#1871, #3352) are what this answers.

## Contents

- [Accounting](#accounting)
- [Reservation and limit](#reservation-and-limit)
- [Uplinks are declared](#uplinks-are-declared)
- [Where each is held](#where-each-is-held)
- [An endpoint's own shaping](#an-endpoints-own-shaping)
- [What it costs](#what-it-costs)
- [Abuse and attack](#abuse-and-attack)

## Accounting

Per-endpoint byte and packet counters are kept in the kernel and sampled into metering, never
reconstructed from averages, since they have financial consequences. Counting in the kernel rather
than in a proxy captures traffic the application never sees or hides, and a compromised workload
cannot evade it.

Every packet to or from a workload's link passes the host's table, and three base chains on the
forward, input and output hooks, at priority -150, send it to the workload's own chains, which count
and set its priority and decide nothing but a budget cut-off. For each endpoint whose `accounting`
is on (the declaration that it is metered; a rule on every packet nobody meters is cost with no
reader), named counters `<link>_<transport>_<port>_in` and `_out` count bytes and packets to and
from its port, in the host's namespace, out of the workload's reach. The workload's egress, what
leaves its link for anything but another workload's link on the node, is counted as `<link>_egress`
whatever its endpoints say, since its [egress budget](quota.md) is measured in it. The meter reads
them through the helper's `ReadCounters` at each reading, one listing of the table, and journals
them with the interval's sample, cumulative and differenced as everything metered is (see
[observability](observability.md)). Counters survive a rewrite of the chains.

## Reservation and limit

`ResourceSpec.bandwidth` has the same split as everything else: a reservation is guaranteed
bandwidth, subtracted from the node's capacity and never oversubscribed; a limit is the ceiling; the
gap is burst, contended fairly. A workload sold "100 Mbit guaranteed, 1 Gbit burst" outbound gets
exactly that at the node's interface, and the number is not shown where the kernel does not enforce
it.

**Inbound has a limit and no guarantee.** A node controls what it sends and can only drop what it
receives; congestion upstream, an attack included, happens before the node's kernel sees a packet.
So `bandwidth.ingress` has a `limit`, policed at the node, and no reservation. An outbound
reservation is a guarantee at the node's interface and says nothing about the network beyond it, and
the interface says so.

A workload with a bandwidth reservation or limit is placed only on a node whose link-joining driver
declares `egress` or `ingress` enforced, against the node's allocatable bandwidth; an endpoint
asking for shaping or accounting only where `endpoint_shaping` or `endpoint_accounting` is declared.
Elsewhere it stays unplaced with that reason. The agent declares each kind on what it rests on:
`endpoint_accounting` wherever the helper gives workloads networks; `ingress` and `endpoint_shaping`
wherever it can also write qdiscs; `egress` only with a declared and shaped uplink.

## Uplinks are declared

`korpis-agent run --uplink eth0=1gbit`, repeatable, names each interface and the capacity it sells,
because the link speed the kernel reports is a NIC's negotiated rate, and on most virtual machines a
fiction; what a host sells is what its provider sold it, which only the operator knows. The node
keeps part of each uplink for itself, five percent unless named (`eth0=1gbit,reserve=50mbit`), as
the default class's floor, so its control stream, journal and relays are never starved by
reservations it sold. With several uplinks each gets the same tree, and the node's allocatable
bandwidth is the smallest uplink's capacity less what it keeps, since a workload's traffic leaves by
whichever its route chooses. A node declaring no uplink shapes nothing and enforces nothing.
`korpis-agent preflight` reports `tc` and HTB as the helper finds them; the node needs `tc` from
iproute2.

## Where each is held

**A reservation is held on the uplinks**, where the node's workloads contend. The helper's
`EnsureUplinks` keeps on each declared uplink an HTB tree whose root rate is the uplink's capacity,
a class per workload with an egress reservation (rate the reservation, ceiling the limit), and a
default class for everything else, the node's own traffic and workloads reserving nothing, whose
rate is what reservations leave. Only a reservation needs a class, since a class is how a guarantee
is held among the contending. The workload's chain sets a packet's priority to its class in the
forward hook, after the kernel's forwarding sets one and before translation, and HTB takes a class
from a packet's priority with no filter. A priority rather than a firewall mark, because marks are
shared with every other program routing by one, and a priority set in the forward hook is Korpis's
alone. Class identifiers are allocated from a map in the host's table keyed by the workload's link.

**The egress limit is held on the workload's own link**, an HTB qdisc on `eth0` in its namespace,
which the helper writes and the workload, holding no `NET_ADMIN`, cannot change: not everything it
sends leaves by an uplink (traffic to the node, its ingress proxy included, never does), and the
ceiling covers it all.

**The ingress limit is held on the host's side of the link**, which everything sent to the workload
leaves, from beyond the node, from its proxy and from other workloads.

Korpis's qdiscs carry handles of their own, `4b50:` on an uplink and `4b51:` on a workload's link,
so they are told apart from another program's and found again with no ledger, and uplink trees are
looked for only among interfaces that are not workload links. Each class sends in turns of 60,000
bytes, where HTB's own quantum would be megabytes at gigabit rates, and each leaf queues by
`fq_codel`.

## An endpoint's own shaping

An endpoint's `shaping` limits what it sends from its port and what is sent to it, each in a class
of its own beneath the qdisc of the workload's link. A link's qdisc is a root of the workload's
limit (or none), a default class, and the endpoints' classes, each with an equal share of the root
as its rate and its own limit as ceiling, so endpoints contend fairly beneath the workload's
ceiling. On `eth0` the class is chosen by the priority the workload's own table sets on what leaves
the port, in an output chain the workload cannot change; a priority it sets on a socket itself can
only put its traffic in another of its own classes. On the host's side, the workload's `to_` chain
sets the priority of everything sent to it, none where no endpoint's class applies, so a priority
another workload's chain set for its uplink class never lands in one of this workload's classes.

## What it costs

Traffic between two workloads of one node never reaches an uplink, so it is held by the sender's
ceiling and the receiver's ingress limit and no guarantee. The mesh is the node's own traffic to its
uplink, in the default class. HTB's precision is the kernel timers', a few percent at gigabit rates,
and a burst within a qdisc's buffer passes at line rate before it is held. The counters cost a rule
per counted endpoint per direction on every packet, and a listing of the table every ten seconds, a
few hundred kilobytes of JSON for a thousand workloads.

## Abuse and attack

Korpis is not a DDoS mitigation provider, but it is the component that knows which workload is
attacked and which is attacking.

| Korpis does | Korpis does not |
|---|---|
| per-source connection and packet rate limits | absorb volumetric attacks |
| SYN cookies, conntrack limits, per-endpoint thresholds | scrub traffic |
| attack signals as metrics and alerts | operate an anycast network |
| null-route an endpoint on operator command or policy | drop a customer's traffic on its own decision |
| an interface for an upstream scrubbing provider | become one |
| detect and rate-limit outbound abuse from a workload | |

Outbound detection is the half operators forget: a compromised game server used as a reflector or
spam relay costs the operator its address reputation, usually learned from an abuse complaint.
Egress rate limits and outbound accounting make it visible within minutes.
