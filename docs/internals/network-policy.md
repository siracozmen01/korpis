# Network policy

Who may connect to a workload, where it may connect out, and how many connections it may hold. No
panel in this market has egress control, and it is the gap that hurts operators most, because a
compromised game server is a launchpad.

## Contents

- [Two policies](#two-policies)
- [Egress defaults](#egress-defaults)
- [How the defaults are held](#how-the-defaults-are-held)
- [A workload's own rules](#a-workloads-own-rules)
- [Rules naming workloads](#rules-naming-workloads)

## Two policies

```
NetworkPolicy        on each endpoint
  ingress   []Rule   who may connect; the first matching rule decides, unmatched is let in
  conntrack          connections to it at once, and new ones a second

EgressPolicy         the workload's own, IntentBody.egress
  rules     []Rule   where it may connect out, before the defaults
  limits             each may lower the default, never raise it
```

Outbound rules are the workload's own, not an endpoint's: a connection a workload opens leaves from
no endpoint, so outbound rules on two endpoints that disagreed would mean nothing any component
could evaluate. An endpoint's policy says who may connect to it and how many, which its node can
judge, since such a connection arrives at its port. The endpoint's old `egress` field stays in the
frozen schema and a declaration setting it is refused, naming where the rules go.

## Egress defaults

Applied to every workload unless deliberately changed. Egress is not default-deny: it denies every
internal destination and allows the public internet, rate-limited.

| Destination | Default | Why |
|---|---|---|
| other workloads | deny | lateral movement between customers |
| the node itself: management, SSH, the agent | deny | privilege escalation |
| the control plane's API | deny | a workload holding authority reaches it through its node's relay socket, not the network (see [authentication](authentication.md)) |
| link-local metadata (`169.254.169.254`, `fd00:ec2::254`) | deny | cloud credential theft: reaching it often reads the instance's IAM credentials and takes the operator's whole cloud account |
| RFC 1918, CGNAT and unique local ranges | deny | the operator's own infrastructure |
| SMTP (TCP 25, 465, 587) | deny | spam is the fastest way to lose an address range's reputation |
| the public internet | allow, at most 200 new connections a second, burst 400 | mod downloads, auth servers, updates |

**Connection tracking limits** cap each workload at 4,096 tracked connections, both directions, and
the new-connection rate above. Without them one workload can exhaust the node's conntrack table and
take every other workload offline, an exhaustion with no visible cause. Half-open connections have
no counter of their own: the rate bounds how fast they are made, and the node turns on SYN cookies,
so a flood costs the table nothing. Connections made to a workload are bounded by the tracked limit,
not a rate, since a game server's players arrive in bursts its operator chose to serve.

## How the defaults are held

Each row is a rule in two base chains of the host's table, on the `forward` and `input` hooks,
judging what a workload's link sends:

| Row | What matches |
|---|---|
| other workloads | anything the node would forward to another workload's link, whatever its pool |
| the node | anything addressed to the host, on any of its addresses, except ICMP to the gateway |
| link-local metadata | `169.254.0.0/16` and `fe80::/10` beyond the gateway |
| private ranges | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `100.64.0.0/10`, `fc00::/7`, loopback |
| SMTP | TCP 25, 465, 587, anywhere |

On one machine the control plane's address is the node's own, which the node row denies; where it is
public, a workload reaches it as any public address, bounded by its authentication alone. Each
denying row counts what it drops under a comment naming it, which is how tests tell this firewall's
verdict from another's and what accounting reads. The shared chains are written once per version of
their definition, so their counts accumulate. They run at priority -10, before filter chains at the
default priority, an operator's firewall among them, so what they deny is dropped whatever another
firewall decides; what they let through another can still drop. A connection to a published endpoint
is a client's, whoever makes it, so one workload reaching another's endpoint through the node's
address passes as a client and the destination's table judges it. What differs per workload, the
destinations its dependencies open and the rate's state, is a chain of its own reached through a map
keyed by its link. Between two workloads on one node both ends hold: the source's chain on the host
and the destination's own namespace. A node whose root is compromised enforces none of this.

## A workload's own rules

A workload's egress rules are in its own chain, after the destinations opened for it and its limits
and before the defaults: a deny drops and counts under `policy`; an allow returns the connection to
the default, which decides the rest. So a tenant's allow cannot open what the default protects (the
node, the operator's networks, metadata, other tenants, SMTP): those rows are the operator's to
relax, and nothing relaxes them until an operator-level policy exists. Its limits may lower the
default's and not raise them, a raise being refused at declaration, since the default is what keeps
one workload from taking the node's table from the others.

An endpoint's rules are in the workload's own table, in a chain its port's connections pass: its
limits first, a connection limit and a rate for the endpoint as a whole, then its rules in order, an
allow letting in and a deny dropping and counting, then anything unmatched let in. They see the
client's own address, which forwarding preserves. The node's health probes are made from inside the
workload's namespace and are judged by none of these rules, since a probe an allow list turned away
would restart a healthy workload. Every rule names an address range or workloads; a rule naming a
domain is refused, since a packet filter cannot judge one. A rule a node cannot hold fails the
workload's network rather than being left out, because a deny left out is a weaker policy than the
one declared. An endpoint's limits protect the workload from a flood, not one player from another.

The defaults are a guess at what a game server or web service needs; a crawler or a proxy opening
more connections needs its policy, and today an operator, to say so.

## Rules naming workloads

An endpoint's rule may name workloads by a label selector, to allow or deny. A workload's egress
rule may name workloads only to deny: an egress allow returns to the default, which denies every
other workload, so it could open nothing, and is refused naming the dependency, which is what opens
one workload to another. An egress deny goes on deciding where an operator's default opens private
ranges. A deny naming a workload the declaring workload depends on does not hold against the
dependency, opened before its rules. The selector matches the rule's own project, as a dependency's
does.

**What it names.** A packet filter judges addresses, so the control plane resolves each such rule,
when it computes the assignment, to the addresses of the matched workloads as their nodes observed
them (`Assignment.peers`), named only where the rule's node would route a packet for that address to
the matched workload: on its own node, or on a node paired with it in the mesh at that node's routed
pools, which are disjoint. A matched workload anywhere else is left out of an allow, since its
private address may be one the rule's node gave a workload of its own, perhaps another tenant's. A
deny also names every global IPv6 address a matched workload is observed at
(`Observation.address_ipv6`), which drops more and never lets more in. A matched workload is named
only while its node observes it starting, running or stopping, when its network exists, so a stale
address its node may have let go is never named. The helper writes a rule per family it names and,
where it names none, a rule matching nothing, which is what it means. A rule's range may be all of a
family (`0.0.0.0/0`, `::/0`), which is how an endpoint says "and no one else" after its allows.

**When it changes.** A workload of the project placed, retired, relabelled or observed at a new
address wakes the node of every workload of the project whose rules name workloads, and the agent
remakes a running workload's network when what it would ask the helper changes. Until told, a rule
holds its last addresses, so a newly matched workload is neither let in nor dropped, and an address
a matched workload left is judged as its; the workload given it meanwhile is still held by its own
node's default and the mesh's admission. The workload's status carries what each rule matched and
left out, with why (`WorkloadStatus.named_rules`).

**What it costs and does not do.** Each change to a project recomputes the assignments of its
workloads whose rules name workloads, in proportion. A rule judges by source address, so it matches
a workload only where the connection arrives from that workload's own address: through a dependency,
on one node or through the mesh, or over a global IPv6 address. A connection through the ingress
proxy arrives from the proxy, and one through another node's port arrives translated to that node's
address; no rule naming workloads matches either.
