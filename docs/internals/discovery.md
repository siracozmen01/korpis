# Discovery

How a workload finds what it depends on, and the resolver every workload uses. A web service must
reach its database without knowing where it is placed or hard-coding an address that changes on
migration.

## Contents

- [Dependencies, not cluster DNS](#dependencies-not-cluster-dns)
- [What a target means](#what-a-target-means)
- [Where it is placed and how it is reached](#where-it-is-placed-and-how-it-is-reached)
- [Injection](#injection)
- [Start order](#start-order)
- [The resolver](#the-resolver)

## Dependencies, not cluster DNS

```
Intent.dependencies[]
  name       "db"
  target     a workload or endpoint by id, or a selector within the project
  inject     env | file | both
  required   block start until ready
```

The usual answer is cluster DNS. Korpis's primary mechanism is explicit declaration, for four
reasons:

1. **The graph is visible**, so the scheduler can place a service beside its database instead of
   discovering the coupling from latency complaints.
2. **Start order is derivable**: a required dependency is ready first, with no crash loop until the
   database appears.
3. **A plan can show the blast radius** ("migrating this database reconfigures three dependents, two
   of which restart") before approval, which implicit DNS makes impossible to compute, since nothing
   records who depends on whom.
4. **It works across every tier**: environment injection reaches a container, a microVM and a VM
   alike, where cluster DNS assumes a resolver the workload cooperates with.

The cost is that an address change reaches the environment only at a restart, which the file form,
rewritten in place, mitigates for workloads that watch it.

## What a target means

A target is in the dependent's own project; one in another project is refused, since reaching
another project's workload would be a grant of its own that no action expresses yet. Each target
resolves to endpoints: a workload to its one endpoint whose exposure is not `none`, refused at
declaration where it has none or several, naming the endpoint form; an endpoint to itself; a
selector, each time the dependent's assignment is computed, to every matching workload by its one
reachable endpoint, one with several left out and said. **What evaluates this:** the control plane,
which holds the intents, labels and endpoints, carrying the result in the dependent's assignment:
each dependency's name, injection, whether required, and each target's workload, endpoint, container
port and protocol.

The workload's status carries each dependency (`WorkloadStatus.dependencies`): each target, how the
dependent reaches it (on its own node, through the mesh, unreached, not placed, the nodes unnamed
since a reader of the workload may not read them), and each workload left out with why; `korpis
status` prints it. It is computed when the workload is read, from the resolution, both placements
and the mesh's pairs.

## Where it is placed and how it is reached

A dependent may be placed on any node from which every target's node is reachable: its own, or a
node paired with it in the [mesh](mesh.md). A required dependency whose target is not placed leaves
the dependent unplaced, saying so; one not required constrains nothing. Placement keeps a dependent
near its targets, not a target near its dependents, so a target placed after a dependent that does
not require it is placed by its own needs.

The address of a target on the dependent's node is its workload address, which that node's agent
allocated. A remote target is not resolved by the dependent's node, which holds no other node's
addresses: the control plane, learning each workload's address from its node's observations, carries
it in the dependent's assignment, and it is reached there, every node's pool being its own.

**Reaching it.** The egress default denies other workloads, so the agent opens each target's
address, port and protocol in the dependent's own chain on the host, counted under the comment
`dependency`. The target's own table accepts its declared port already, and its endpoint's own rules
still hold. The connection is forwarded from one workload's link to the other's, so an operator's
firewall dropping forwarded traffic drops it too until it accepts Korpis's links.

## Injection

Into the environment at creation: `KORPIS_DEP_<NAME>_HOST` and `_PORT` of the first target in the
order of the targets' names, and `_ADDRESSES`, every target as `host:port`, comma-separated. Into a
file, `/run/korpis/deps/<name>.json`, in the workload's own read-only directory beside its identity
socket: every target with its workload, endpoint, host, port and protocol, rewritten whole whenever
it changes, so a workload watching it follows a change without a restart. A dependency not required
and with no target now is injected as none: no variables, a file with no addresses. A selector is
evaluated when the assignment is computed, which any change on the node or the target's placement
triggers.

## Start order

The agent creates the dependent's object only once every required dependency's targets run and,
where they have a readiness probe the node runs, are ready; until then the dependent is `starting`,
and its status names the dependency it waits for. This holds for a plan's `create` and `start`
steps, the step left outstanding, as for the node's own re-creation. **What evaluates this:** the
agent, which observes both workloads; for a target on another node, from the target's observed state
the control plane carries.

## The resolver

Every workload resolves names somewhere, and the egress default denies `169.254.169.254` (which on
GCP is the instance's DNS resolver) and private ranges (where AWS puts the VPC resolver), so a
workload sent to the provider's resolver would have none.

**Every workload gets the node's resolver at `169.254.1.53`**, an address on its own namespace's
loopback, named alone in the read-only `/etc/resolv.conf` its container is given; a guest is given
the same address on its config drive, and `korpis-guest` writes the file before the root is made
read-only. The helper opens a UDP socket and a listening TCP socket on port 53 there inside the
namespace and hands them to the agent (`OpenResolver`, as `SCM_RIGHTS`); the agent answers on them
from the host, since a socket keeps the namespace it was made in. So a query never leaves the
workload's namespace and no firewall on the host, Korpis's or another's, stands between a workload
and its resolver: redirecting queries to the host instead would have had every host firewall's input
chain judge them, and on a host dropping input by default, as `ufw` does, no workload could resolve
a name. The agent knows who asked by the socket, needs no privilege and binds nothing on the host.

It forwards each query, over the transport it came on, to the resolvers in the host's
`/etc/resolv.conf`, a local stub included, so the node and not the workload talks to the provider's
resolver, and the metadata address stays denied. It allows each workload 50 queries a second with a
burst of 200, answering beyond that with `REFUSED`, which a resolver library treats as a failure to
retry, and an upstream silent for two seconds with `SERVFAIL`. An agent that restarts asks for every
running workload's sockets again; a workload's are closed when its network is released, since they
keep its namespace alive. A workload whose software ignores `/etc/resolv.conf` and asks a public
resolver reaches it, as egress allows; one asking a private resolver is denied.
