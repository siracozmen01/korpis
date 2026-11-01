# The control plane

What runs inside `korpis-server`, what authorizing every request costs, and what fails how. The
protocol it speaks to nodes is on [reconciliation](reconciliation.md); its store is on
[state](state.md).

## Contents

- [Components](#components)
- [What authorizing every request costs](#what-authorizing-every-request-costs)
- [High availability](#high-availability)
- [Failure modes](#failure-modes)

## Components

Each component has one responsibility and reaches around no other.

| Component | Responsibility |
|---|---|
| gateway | terminates client connections, authenticates the subject, evaluates grants, and rejects anything unauthorized before it reaches the model: the single authorization path |
| planner | computes a plan from a proposed declaration: steps, reversibility, impact. Pure: it reads state and changes nothing |
| scheduler | chooses a node, honoring constraints, quotas, capabilities and placement policy, and records why ([placement](placement.md)) |
| dispatcher | owns the agents' streams: publishes assignments, answers leases, ingests observations and journals. Stateless: any replica serves any agent |
| store | the authoritative state ([state](state.md)) |
| relay | joins client sessions to nodes, holding nothing ([relays](relays.md)) |
| content | resolves recipe references to digests, verifies signatures, tells nodes what to prefetch ([recipes](recipes.md)) |

The planner being pure makes dry-run free, makes plans testable without a cluster, and rules out the
class of bug where computing a plan has side effects.

## What authorizing every request costs

Authorization is a search for a permitting chain: a subject holds grants directly and through N
external identities, each chain d links deep, each link's validity derived, and selector scopes
evaluated against the current workloads. That is O(N x d) reads plus selector resolution per
request, and every plan computation, so every click, is authorized. Caching collides with immediate
revocation: a cache invalidated by TTL makes "immediate" mean "eventually".

So the closure is materialized and invalidated by event, not clock:

| | |
|---|---|
| materialized | per subject, the closure of (action, scope, conditions), each entry carrying the grant and binding identifiers its chains pass through |
| invalidated by | a grant issued, revoked, expired or suspended; a binding changing state; a selector's membership changing |
| recorded by | the transaction making the change increments one store-wide `authz_version` and appends an `authz_change` row naming the identifiers affected |
| learned by a gateway | from the snapshot every request already reads; `NOTIFY` is only a prefetch hint |
| on a miss | full chain evaluation, what a cold gateway always does |
| never cached | use-limit redemption, MFA, approval |

Every API request opens a read transaction for the objects it acts on, and the gateway reads
`authz_version` in that same snapshot. A closure computed at that version is exact for the request,
with no staleness window. If the version moved, the gateway reads the change rows between, drops
every cached entry whose identifiers include an affected grant or binding, and re-evaluates those. A
revoked grant's descendants drop out because their chains contain its identifier, with no subtree
walk. So no request whose snapshot includes a revocation is authorized by a closure that predates
it; a gateway that missed every notification is colder, not stale. Change rows are kept long enough
to cover the oldest closure any gateway may hold, and an older cache is discarded whole. Selector
memberships are materialized beside the closure, so a workload gaining a label is an invalidation,
not a silent widening.

The cost is one indexed single-row read inside a transaction the request opens anyway, plus a range
read when authority changed. When the store is unreachable no request can be authorized, so the API
fails closed; established relay sessions continue on their tokens.

## High availability

The control plane is stateless: every replica serves any client and any agent, agents connect
through a load balancer with no affinity, and there is no leader. State lives in the store, the only
component needing replication. Leases are held in the store with fencing epochs, so failing over a
replica cannot make two authorized writers.

Control plane availability matters less here than in comparable systems, because the data plane
survives a total control plane outage: losing it costs management, not availability. So high
availability is added when needed, not a precondition of the first release.

## Failure modes

Each is a state the system represents and recovers from without manual database surgery, the
standard Pterodactyl fails (issues #4505, #3332).

| Failure | Behavior |
|---|---|
| agent process dies | workloads keep running; on restart the agent adopts them and resumes its journal from the cursor |
| node reboots | workloads restart per their assignments; adoption finds what systemd already restarted |
| partition, node side | the agent keeps converging to its last assignments and journals; `keep_running` leases persist, `stop` workloads stop at the holder's end of the term |
| partition, control plane side | the node is unreachable and its observations unknown; nothing new is placed there; `keep_running` workloads are not rescheduled automatically; `stop` workloads are, once their lease is over, onto a node reaching all their data, by a remake plan |
| node returns after its workloads moved | its refusals name the new holder; it stops them, keeps their data, and reports the volumes as orphans |
| control plane replica dies | agents reconnect elsewhere; nothing is lost |
| relay dies | new sessions use another relay; sessions on it end and resume from their last offset |
| store unavailable | the API fails closed; agents keep converging and journaling; relay sessions continue on their tokens |
| a plan fails halfway | completed steps are effects and their resources are kept and named; the plan is failed and resumable from its last completed step, never recomputed, which would discard the record of what was half done |
| migration interrupted | the source keeps the lease until cutover completes and is verified; the destination's partial copy is collected; one node owns the workload throughout |
| two agents claim one workload | impossible: one epoch is valid at a time, and another node gets one only once the old lease is provably over |
| an agent is compromised | it holds its own node's key, digest-scoped pull tokens and its own workloads' job credentials, and obtains only specs its drivers accept |
