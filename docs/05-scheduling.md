# Scheduling, Placement, and Migration

**Status:** design **Date:** 2026-08-07 **Depends on:** [`01-model.md`](./01-model.md),
[`02-architecture.md`](./02-architecture.md), [`04-runtimes.md`](./04-runtimes.md) **Resolves:**
open questions 1, 4, 5, 7 of `01-model.md`; 4, 5 of `02-architecture.md`; 1 of `04-runtimes.md`

---

## 1. The gap being filled

No product in this market schedules. Pterodactyl, Pelican, Calagopus, AMP, TCAdmin, Multicraft,
WISP, in every one of them, a human picks which node a workload lands on, from a dropdown. Nodes
and locations are buckets, not capacity.

The consequences are the daily reality of running one of these: nodes drift into imbalance and
nobody notices until one is on fire; adding hardware does nothing until someone manually moves
things onto it; decommissioning hardware is a spreadsheet exercise; and the answer to "why is this
customer's server on that node" is "because someone chose it in 2023."

Agones (a Kubernetes extension aimed at a different audience entirely) is the only thing nearby
with an explicit placement policy. Its vocabulary is worth taking; its operational cost is not.

---

## 2. Placement

Three phases. The output is a `Placement`, a `Lease`, and a recorded explanation.

### 2.1 Filter: which nodes *can* run this

A node survives the filter only if all hold:

| Check | Source |
|---|---|
| offers a driver implementing the requested tier | `Node.drivers[].Capabilities.tiers` |
| that driver's declared capabilities admit the intent | `Node.drivers[].Capabilities`, evaluated in the control plane (below) |
| that driver runs a command inside its workload, where a probe of the intent does (an `exec` probe), gives it its identity socket, where a probe of the intent hears from it (a `heartbeat` probe), and asks its guest's agent, where a probe of the intent does (a `guest_agent` probe) | `Node.drivers[].Capabilities.exec`, `.identity_dir`, and `.guest_agent`; added 2026-10-23, when probes were built, as the `probe` check: no probe is accepted that its node would never run, or that its workload could never answer (§5 of `04-runtimes.md`) |
| for the shortlist, the driver's `Prepare` accepts it | a `FeasibilityQuery` (§4.2 of `02-architecture.md`), side-effect free |
| has unallocated capacity for every requested dimension | `Node.allocatable` minus `allocated`; among them `node_ports`, the ports of its range its `node_port` and `node_bound` endpoints hold (§3 of `07-networking.md`); an endpoint resolved to `stable` holds an address and port of its edge group instead, and none of its node's (designed 2026-10-26, §3.1 there) |
| has every requested device available | §6 |
| satisfies the intent's constraints | `PlacementSpec` label selectors, required affinity rules |
| offers every storage class the intent's volumes name, and for a volume asking for replicas a class able to send increments, the one it names or the node's default (§2.2 of `06-storage.md`; added 2026-10-27) | `Node.storage_classes` |
| holds every existing volume the intent attaches | `Volume.node` (§7 of `06-storage.md`); added 2026-10-14, when attaching was built: a node-local volume is on one disk. Designed 2026-10-27 for storage over the network (§2.1 there): a volume in a shared class is held by every node offering a class of its `store`, and one in a block class by every node offering its class; built the same day for shared classes |
| the workload tolerates every taint | `Node.taints` |
| is `ready`: active and connected, so not cordoned, draining, or unheard | `Node.admin`, `Node.connectivity` (§3.4 of `01-model.md`) |
| belongs to the workload's organization or one of its ancestors | `Node.organization` (§3.4 of `01-model.md`) |
| can be reached the way the workload's endpoints need | measured reachability (§9 of `07-networking.md`); an `ingress` endpoint needs the node to serve the ingress (§3.2 there) unless an edge group serves it, which the placement decides as it decides `stable` (§3.1 there; corrected 2026-10-27, Finding 197 of `23-walkthroughs.md`: a group serving the `stable` endpoints alone excused the `ingress` ones, which nothing then served) |
| is in the overlay, when a dependency or a dependent is placed on another node | `Node.overlay` (§9 of `07-networking.md`); built 2026-10-15 as the `dependency` check, with no node in the overlay yet: a dependent goes where its targets are, and a dependent does not yet hold its targets (§5 of `07-networking.md`). Designed 2026-10-25 with the mesh (§9.1 there): the check asks the pair, `Node.peers`, so a dependent may be placed on a node from which each target's node is reachable, and targets on two nodes are placeable. Built 2026-10-25: the request carries the pairs of members that reach each other, and a placed target on another node refuses a candidate that does not reach its node |
| a pool that serves the workload reaches it with a free address, or the address the workload holds reaches it, where an endpoint of it is `dedicated_ip` | the address pools and their addresses (§3.7 of `07-networking.md`); designed 2026-10-26 as the `dedicated_ip` check: a `node` pool reaches its one node, a `cluster` pool every node of its own organization, and the address is allocated in the placement's own transaction. Built 2026-10-26 for `node` pools: the request carries the nodes the workload may hold an address on, those its held address's pool reaches or a pool serving it reaches with one free, computed under the placement lock; a placement by hand is refused it too, as a node that cannot run the workload is |
| reaches every member of the edge group through the mesh, where an endpoint of it resolves to `stable` or a group serves its `ingress` endpoints | `Node.peers` (§3.4 of `01-model.md`); designed 2026-10-26 as the `edge` check (§3.1 of `07-networking.md`): the endpoint resolves to `stable` where a group of two members or more serves the workload with an address and port for each such endpoint, which the control plane allocates in the placement's transaction; where none does, to an address of a `cluster` pool serving it, which the `dedicated_ip` check then asks for; and otherwise to `node_bound`, which the reachability check then judges; the node is asked what a dependency's is, that the pair with each member is reachable, since the group's tunnel crosses it. Built 2026-10-26, the request carrying the serving group's members and the filter asking the mesh's pairs, the allocation in the bind's transaction; and the `cluster` pool's address where no group serves, the request saying the endpoints are routed and the `dedicated_ip` check asking for that pool's address (P3.7e3); and a group serving the `ingress` endpoints, its members asked of the node as a `stable` group's are, since each member's proxy reaches the workload through the mesh (P3.7e5) |
| every quota entry counting what runs on the node admits it | §5.1, "Where a workload runs" |

**`allocatable` is measured by the agent, less a reserve the operator sets on the node.** The
agent reports capacity as the machine has it (a thousand millicores per core, and total memory)
and `allocatable` as capacity less a system reserve: by default 500 millicores and 1 GiB, each
capped at a quarter of capacity so a small machine is not reserved away, and set per node with
`korpis-agent run` (§3 of `18-operations.md`). The reserve is what the machine's own system, the
agent, and the runtime need; without it a node fully reserved on paper is overcommitted in fact.
It limits nothing an operator may run (P10): it is a measurement the operator corrects, not a
gate. The cost is a default that is wrong for an unusual machine.

Disk is reported from the storage classes the helper declares (§2 of `06-storage.md`): capacity
is their size, and `allocatable` what is free in them when the agent starts, with no reserve,
since the filesystem's own free space already leaves out what the machine has written. Everything
already on those filesystems counts as taken, this node's own volumes included, so after a
restart a node reports less room than it has and never more. The cost is that conservatism: a
node with large volumes that are mostly empty is offered fewer new ones than it could hold, until
quota accounting lets the agent subtract what its own volumes reserve instead of what they use.

> Added 2026-10-13, from the implementation. `allocatable` was defined in §3.4 of `01-model.md`
> as "capacity minus system reserve", and nothing said what the reserve was or who set it.
>
> Corrected 2026-10-13, from the implementation. This section said disk joined the report with the
> volumes of Phase 1, while the scheduler already counted a volume's size against the node's disk
> and Phase 0 creates volumes. Every workload with a volume was unplaceable: the first run of a
> real recipe on a real node, in `crates/korpis-e2e`, was refused with "needs 67108864, 0 free".

Asking the driver before committing is the part other systems lack, and it happens in two layers,
because the scheduler runs in the control plane and `Prepare` runs on the node.

**First, declared capabilities, evaluated where the scheduler is.** Every driver publishes in
`Hello` and `NodeStatus` what it can do as data: tiers, architectures, image and disk formats it can
run, whether nested virtualization is available, the Landlock ABI, kernel features, volume
semantics, device modes (§4.1 of `04-runtimes.md`). The Content component resolves the recipe's
manifest, which names its architecture, format, and requirements, without fetching any layer. The
filter compares the two. This rejects the large majority of unsuitable nodes with a reason and
costs no round trip.

**Then `Prepare`, for the shortlist only.** Some questions are not answerable from declared data
(does this specific configuration validate against this driver's rules). For the few candidates
that survive scoring, the scheduler sends a `FeasibilityQuery` carrying the intent and the resolved
manifest, and the driver answers with `Prepare`'s result. `Prepare` is pure (§4.2 of
`04-runtimes.md`), the manifest is supplied so nothing is fetched, and the query has a timeout; a
node that does not answer in time is treated as rejecting, with that reason recorded.

**The query is asked outside the placement lock, before the transaction that binds.** The control
plane scores the nodes as the store holds them, without the lock, and asks the best three at once,
waiting five seconds for all of them together. Each query carries a random identifier, and an
answer is taken only from the node it was sent to. Only an agent that says in its `Hello` that it
answers is asked (§4.2 of `02-architecture.md`); one that does not, built before the query, is
ranked and bound unasked, rather than passed over for a silence that would never end. The bind then
runs as before, under the lock in the approving transaction (§2.3), and filters and scores again
with what the query found: a node whose driver refused is rejected by the check `prepare` with the
driver's reason, and a node that did not answer is rejected by the same check with that said. A
refusal is kept for the target, so every later bind of that target, a retry or an operator's choice
of node (§2.3), leaves the node out; a new target is asked again. A node that did not answer
decided nothing, so it is passed over for that bind alone and asked again by the next. Because the
bind filters again, a node that changed between the query and the bind is judged as it is when
bound, and a node the query never reached, because the three best all refused or because the
ranking moved, is bound unasked and checked by `Prepare` after the bind, below. The query is asked
when a target is approved, at declaration or by a person, and when the retry pass tries an unplaced
workload again; a workload that is already placed follows its target on its node or not at all
(§8), so nothing is asked for it.

What it costs. An approval waits up to five seconds when a shortlisted node is silent, and a node
that answers slowly is passed over though it might have accepted. A driver that refuses the same
target on one day and would accept it on the next, after an upgrade, is left out of that target
until the target changes; the operator's remedy is declaring again. **A node whose session this
process does not hold is not asked**: the control plane asks over its own sessions, so when
several control-plane processes share an installation (§5 of `18-operations.md`), a node
connected to another is ranked and bound unasked, and only the check after the bind covers it.

**And `Prepare` runs again after binding, on the bound node,** as the first thing the agent does
with a new assignment, because the node's state can change between the query and the bind. A
rejection there is the `unsatisfiable` observation of §7, and the scheduler re-places with that
node excluded and the reason in the `Explanation`.

What the scheduler reads is the plan step that failed. When the driver refuses before anything of
the workload was made on the node, no step of its plan completed there and no runtime object
exists, the agent journals the failure with the reason `step.refused` rather than `step.failed`;
only the agent knows that nothing was made. Ingesting it, the control plane keeps the refusal for
the target, as the shortlist's are kept, undoes the placement, and places the workload again with
that node left out. The plan stays open, so it runs from its first step wherever the workload
lands, or waits, unplaced with the full explanation, until a node can take it. Two cases are not
moved. A workload an operator placed by hand stays, and its plan fails, since the scheduler does
not overrule that choice (§2.3). A workload that already has something on the node, a volume a
step made or a runtime object, has its refusal journalled as `step.failed`, and its plan fails
there.

What it costs. A workload whose node refused after a step made its volume stays on that node,
failed, until its driver accepts or an operator acts, because moving what it has there is
migration (§8). And a node that refuses after the bind has already cost the workload one round
of its plan's first step, which the shortlist exists to avoid.

> Second external review, finding S13. Recorded in §17 of `23-walkthroughs.md`. The first version
> called `Prepare` from the scheduler "for every candidate", described it as free to ask, and the
> agent protocol had no message that could ask it; trace §1 of `23-walkthroughs.md` ran it after
> binding instead. It also proposed checking "an image format" on nodes that did not have the image.
> The capability filter, the shortlist query with a supplied manifest, and the post-bind check are
> what "rejected at scheduling time with a reason" actually requires.

> Built so far, 2026-10-13. Phase 0 builds the first layer and half of the third: the declared
> capabilities are evaluated in the control plane, and the agent runs `Prepare` before it creates
> anything and reports a rejection as `unsatisfiable` with the driver's reason (§7). It does not
> build the shortlist `FeasibilityQuery` or the re-placement after a post-bind rejection. Both
> are in the schema (`FeasibilityQuery` and `FeasibilityResult` in §4.2 of `02-architecture.md`,
> under rule E of `20-roadmap.md`) and arrive with Phase 1's scheduling (§4 there). Until then a
> workload whose bound node's driver rejects it stays bound there, shown as unsatisfiable with
> the reason, and nothing moves it, since moving a placed workload is migration. Whether the
> shortlist query runs inside the placement lock is question 9 of §10.
>
> Updated 2026-10-15: the shortlist query is built, as above, and question 9 is resolved. The
> re-placement after a post-bind rejection is built too, as above.

### 2.2 Score: of those, which is *best*

```
PlacementPolicy
  strategy      packed | distributed | balanced
  spread        []SpreadConstraint
  affinity      []AffinityRule
  weights       map[Signal]float
```

**Strategy**, following Agones' vocabulary because it is the right one:

| Strategy | Prefers | Right when |
|---|---|---|
| `packed` | the fullest node that still fits | elastic capacity, consolidation enables scale-down |
| `distributed` | the emptiest node | fixed hardware, minimize blast radius |
| `balanced` | evens utilization | mixed, and the default |

Utilization is measured after the placement, per reserved dimension the node declares (CPU,
memory, disk, egress bandwidth: what §5.2 guarantees), as the fraction of `allocatable` held.
`packed` scores the mean of those fractions, `distributed` one minus the mean, and `balanced` one
minus the largest: it prefers the node whose most loaded dimension stays lowest, which evens
utilization across nodes and across dimensions at once, so a memory-heavy workload does not land
on the node whose memory is already the bottleneck.

**Spread constraints** are orthogonal to strategy and operate on topology labels:

```
SpreadConstraint
  topology_key   "rack" | "region" | "power_feed" | "host"
  max_skew       int
  scope          project | organization | label_selector
```

"No more than one of this customer's three database replicas per rack" is a spread constraint. It
is independent of whether the cluster is packing or distributing, which is why it cannot be folded
into the strategy, and why Pterodactyl's Location, a rigid two-level hierarchy, could never express
it.

**A topology domain is a node label's value.** A node's domain under `rack` is its `rack` label;
`host` is the node itself unless it carries a `host` label. A node without the label is in no known
domain, so nothing can show that a required rule holds there: it is rejected by required affinity
and anti-affinity alike, and scores zero for a preference. Treating it as a domain of its own would
let an unlabelled node in the same rack satisfy "apart by rack".

**What a spread constraint and an affinity rule count is the placed workload's own
organization.** `project` counts the workloads of its project, `organization` those of its
organization, and `label_selector` those of its organization that the selector matches (and
constrains only a workload it matches itself). An affinity rule's selector is read the same way.
None of them reaches another tenant's workloads, which is what keeps the evaluation inside what §8
of `08-identity.md` lets an explanation show, and stops a tenant from steering its placement by
another tenant's labels.

**Spread is a score, not a filter.** A node whose domain would exceed `max_skew` scores zero for the
failure-domain signal rather than being rejected, so a workload is still placed when only a skewed
domain has room. The cost is that `max_skew` is a strong preference, not a guarantee; a constraint
that must hold is a required anti-affinity rule, which is a filter (§2.1).

**Where the policy comes from.** One policy per installation, set in the control plane's
configuration, with `balanced` and every weight 1 as the default. Whether an organization may set
its own, and how two such policies combine over a node both may use, is question 8 of §10.

> Decided 2026-10-22, resolving question 8 of §10 before Phase 2 was called done (§4 of
> `20-roadmap.md`). **An organization may set a placement policy for its subtree, and the policy
> is read in two halves, each from the party whose decision it is.** The strategy and the
> weights say how a pool of machines is used, consolidated or spread thin, and that is the
> decision of whoever owns the machines: a node is scored by the policy nearest its owner, the
> organization it was enrolled into (§3.4 of `01-model.md`) or the nearest above it that set one,
> and the installation's where none did. The spread constraints and the affinity rules say how a
> tenant's workloads stand towards one another, and that is the tenant's: a workload is placed
> under those of the policy nearest its own organization, or the installation's. So a host packs
> its shared fleet and distributes the machines it enrolled into one customer's organization,
> each by the policy set there, and a reseller spreads its customers' servers across racks
> wherever they run.
>
> **Candidates are compared within one owner at a time, nearest the workload first.** A node
> serves its owner's subtree (§2.1), so every candidate's owner is the workload's organization or
> one above it. The survivors of the filter are ranked by how near their owner is, the
> workload's own organization first, and by score within one owner; a farther owner's node is
> chosen only when no nearer owner's node survives, and stays on the shortlist behind them, so a
> node that refuses at prepare passes the placement on. Within one owner every node is scored by
> one policy, so no two policies' scores are ever compared: a weight is a statement about the
> signals of one pool, not a bid against another's. A customer who enrolls machines of its own
> runs on them while they have room, which is what enrolling them is for, and a host's dedicated
> pool takes its customer's workloads before the shared fleet does.
>
> Who sets it: an organization's own policy is set, replaced, or removed under `org.write` over
> it, as its delegation ceiling is, recorded as an effect, and read under `org.read`, with the
> policy in force there. A policy set nowhere below the root leaves everything as it was: the
> installation's. The explanation records the workload's policy, as it did, and the policy each
> owner's nodes were scored by, both shown to a viewer who may read every node the explanation
> names (§8 of `08-identity.md`).
>
> What it costs. A policy is one object, taken whole from the nearest organization that set one,
> never merged field by field, so an organization that sets only a strategy also sets, for its
> subtree, empty spread constraints; a field-by-field merge would make the policy in force a
> composition nobody wrote. A workload whose own organization's node can run it lands there even
> when a host's node would score far better, by volume locality or spread; a tenant that wants
> otherwise does not enroll the node, or taints it. And a spread constraint counts the workloads
> in every owner's nodes, while ranking prefers the nearest owner, so a constraint the nearest
> owner's nodes cannot satisfy is outranked by ownership, as any preference is by a filter.
>
> Built the same day: `Policies` in `crates/korpis-core/src/schedule/policy.rs` and the ranking in
> `score.rs`, the organization's column (migration 0122), `GetPlacementPolicy` and
> `SetPlacementPolicy`, and `korpis organization placement` (§7 of `13-surface-cli.md`). A policy
> decides the placements made after it is set and moves no workload already placed. **A policy
> states at most sixteen spread constraints and thirty-two affinity rules, and an intent at most
> thirty-two affinity rules** (Finding 143 of `23-walkthroughs.md`): placement evaluates every one
> against every candidate and the workloads it selects while it holds the installation's one lock
> (§2.3), so an unbounded list let one tenant hold every other's placements for as long as it
> chose. The installation's own, from its flag, is held to the same bounds.

**Scoring signals**, weighted:

| Signal | Prefers |
|---|---|
| utilization fit | per strategy |
| content locality | a node that already has the recipe digest, start latency instead of a download (Rule K-15) |
| volume locality | the node already holding this workload's volumes; usually decisive, since moving them is expensive |
| device fit | not consuming a GPU node for a workload that needs none |
| failure domain | per spread constraints, preferred affinity rules, and the intent's preferred region |
| recent churn | penalize nodes that recently lost workloads, they may be unhealthy |

Each signal scores a node from 0 to 1 and is multiplied by its weight (a quarter for content
locality and 1 for every other signal, unless the policy says otherwise); the highest sum wins, and
a tie goes to the lower node identifier so that the same inputs always choose the same node. **A
signal is scored only where its information exists.** Content locality needs the node's content
inventory, which agents report as `ContentStatus` (§4.2 of `02-architecture.md`), and recent churn
needs the node's history of losses, which the control plane keeps itself (below; corrected
2026-10-15: this said the crashes its agents observed count, and they do not). Until that
information exists, the explanation omits the signal rather than scoring a guess.

> Corrected 2026-10-13, from the implementation. This said agents report both. No agent message
> carries a history of lost workloads, and none needs to: every loss is already an observation
> or a placement change the control plane recorded. Neither signal is scored yet. Both arrive
> with Phase 1's scheduling (§4 of `20-roadmap.md`); what window churn is counted over is
> question 10 of §10. Volume locality applies only to
a workload already placed, and failure domain only when a constraint, a preference, or a preferred
region applies to the workload.

**Content locality, as built.** A recipe's content is held on a node once a `pull_recipe` step
fetched it there, and the agent keeps the digests of those recipes on disk and reports them as
`ContentStatus`, all of them, on every new session and whenever one is added; each report replaces
the last. The signal is 1 for a node that reported holding the recipe the intent names and 0 for
one that reported without it, and a node that never reported is not scored for it. Its weight is a
quarter, so it decides between nodes whose utilization is within a quarter of each other and never
overrides the strategy (question 3 of §10). What it costs. Content is matched by the recipe's
digest, not by the image layers it shares with others, so a node holding version 1.20 of a recipe
is not preferred for 1.21 even when most of the bytes are the same. Nothing on a node removes
fetched content yet, which is what keeps the inventory true; whatever comes to remove it must
remove the recipe from the inventory too, or the node is preferred for content it no longer has.
And a node can report content it does not hold. The control plane takes the node's word, as it
takes its word for its capacity, and the cost is a slower start of what is placed there, not a
breach: a report moves only a score, and a node is trusted with what it runs in any case.

**Churn, as built.** Churn counts the node's loss events, not its workloads' crashes. A loss is
recorded when the silent-node sweep marks a node unreachable while it holds at least one placement,
one event however many it held, so a power cut counts once however full the node was, and not at
all on a node that held nothing. A workload's crash is left out: it says more about the recipe than
the machine, the agent's restart backoff already answers it, and counting it would steer every
placement away from a good node running one bad recipe. A loss weighs 1 when it happens and halves
every day, and is forgotten after a week; the signal is 1 / (1 + the sum), so 1 for a node that
lost nothing and a half for one fresh loss, and it is scored for every node, since the control
plane records every loss itself (question 10 of §10). Neither an agent restart that reconnects
within the sweep's staleness nor a control plane restart is a loss: the first is never marked
unreachable, and the second marks nodes `unknown`, not unreachable. What it costs. A partition that
silences a healthy node counts the same as a dead one, since the control plane cannot tell them
apart; a node lost once is passed over for about a day even when the cause was fixed in a minute,
unless the operator lowers the signal's weight; and when evacuation arrives (Phase 3), a placement
taken away because a node failed follows the same silence, so it adds nothing the sweep has not
already counted.

### 2.3 Bind: and record why

```
Placement
  workload, node, bound_at, bound_by, reason
  intent        IntentID                    // the approved intent it was bound for
  reserved      Resources                   // that intent's reservations, held on the node
  explanation   Explanation
```

```
Explanation
  candidates     int
  filtered       []{node, check, detail}     // every rejection, with the reason
  scored         []{node, signal, value, weight}
  chosen         NodeID
  runner_up      NodeID?
  policy         PlacementPolicy             // as evaluated
```

**Every placement decision is explainable after the fact.** "Why is this workload here" returns the
full derivation: nineteen nodes considered, eleven lacked the `microvm` tier, four failed the
memory check, one was cordoned, three were scored, this one won on volume locality.

This is not a debugging luxury. A scheduler nobody can interrogate is a scheduler nobody trusts,
and an untrusted scheduler gets overridden manually until it might as well not exist. It also
answers the support question that consumes hosting operators' time: *why is my server slow / here /
not starting.*

Manual placement remains available (`bound_by: operator`) and is recorded as an override with the
reason the operator gives. The scheduler is not a wall.

What the override reaches, decided 2026-10-13 when it was built (`WorkloadService.PlaceWorkload`,
`korpis workload place`). An operator binds a workload whose approved target has no node to the
node they name. The choice overrides the node's cordon and taints, because those record the
operator's own decisions about the node, and nothing else: every other check of §2.1 is a fact
about whether the workload can run there (a driver, a tier, capacity, a storage class, the overlay)
or its tenant's own declaration (constraints, affinity) or tenancy (the node's organization), and a
node failing one is refused with `UNSATISFIABLE` naming each check, whoever chooses it. A draining
or decommissioned node is not overridden: draining is the operator emptying it. It needs
`node.place` on the node and `workload.write` on the workload, and the effect names the
`node.place` grant and the reason. A workload already on a node moves only by migration (§8), so
placing it elsewhere is refused, naming `korpis workload move` (§8.1). (Corrected 2026-10-28, as
migration's proposal was built: this was refused as not yet built.) The cost is that an operator
cannot force a workload onto a node it does not fit, even when they know better than the
measurement; the honest remedy is to correct the measurement, not to bind past it.

**When a placement is bound.** In the transaction that approves a target (§3.3 of `01-model.md`),
whether by default policy or by an approver, so an approved workload either has a node or a
recorded reason it has none before the approval commits. A scheduled window does not delay it:
the capacity a change will need is held from the moment it is approved. Every bind holds the
installation's placement lock from reading the nodes to writing the placement, and so does every
cordon, so two binds never count the same free capacity and nothing lands on a node cordoned while
the scheduler was choosing. The cost is that binds are serialized across the installation; at
Phase 0 rates that is unmeasurable, and whether it stays so is question 4 of §10.

**A placed workload follows its target in place, or not at all.** When a new target is approved
for a workload that already has a node, only that node is considered, with the workload's own
reservation counted as free for it, and a cordon does not stop it: cordoning stops new
placements, not changes to what is there. If the node can hold the new target, the placement
follows it. If it cannot, the placement stays bound for the old target, the workload is unplaced
for the new one, and the explanation says why. Moving it is a migration (§8), which has a plan of
its own; an approval never moves a workload as a side effect.

**When no node can take it, the attempt is kept.** The newest failed attempt is stored with its
full explanation, the workload shows as unplaced (§4.3 of `02-architecture.md`), and the control
plane retries every such workload on a short interval, so a node enrolled, uncordoned, or freed is
found without anyone approving anything again. The failure is logged once per target, not once per
retry, and a retry that reaches the same explanation changes nothing, so the record says since
when this has been the reason and a watcher hears when the reason changes, not every retry pass. A
cancellation that puts back the target a workload is placed for forgets the failure recorded for
the target it withdrew.

### 2.4 Failover: placing again a workload whose node is gone

**Designed 2026-10-27** (P3.8a in §4 of `20-roadmap.md`; trace 80 of `23-walkthroughs.md`). The
rule was already written, in the failure table of §9 of `02-architecture.md` and step 5 of trace 3:
a workload whose `on_lease_expiry` is `stop` is placed again once the grantor's end of its lease
has passed, and a `keep_running` one never is (§4.5 there). Nothing did it; this is what does.

When the silent-node sweep has marked a node unreachable, the control plane takes each workload
placed there whose resolved `on_lease_expiry` is `stop`, and once the grantor's end of its lease
has passed, the receipt of its last renewal plus the term, places it again: the filter of §2.1 and
the score of §2.2, the unreachable node failing the filter as any node not `ready` does, bound in
one transaction under the placement lock with `bound_by: failover`, and recorded as the effect
`workload.failover`, naming the node it left, the node it went to, and what it took with it. A
workload no node can take stays placed where it was, its status saying why, and is tried again at
each sweep. The node's reservations for it are released with the placement.

A workload can be placed again only where all of its data is: it has no volume; or each volume is
in a shared class, whose store the new node offers (§2.1 of `06-storage.md`); or in a block class
the new node offers, whose provider fences the old node at the attach; or it is replicated and the
workload declares `continuity.replica_failover: automatic`, and the new node holds its freshest
replica, whose age the effect records as what was lost (§2.2 there). A workload with a node-local
volume and no such replica is not moved, since its only copy is on the node that is gone, and its
status says so (§5 of `18-operations.md`); an operator's `evacuate` may still start it elsewhere
from its latest backup or empty, its plan naming the loss (§3).

**What evaluates "the old node is no longer running it", where, and with what information:**
nothing observes it, since the node is silent. The control plane holds when it received the last
renewal of the lease and the term, and the holder stops a `stop` workload by its own clock before
that end (§4.5 of `02-architecture.md`), so once the end has passed a working agent has stopped it;
for a block class the provider also refuses the old node at the new attach, which holds even
against an agent that is hung rather than cut off. A shared class has only the first, which its
`fencing: lease_only` says.

What it costs. A failover waits for the sweep's three missed heartbeats, ninety seconds by default,
by when a sixty-second lease last renewed before the silence has ended too, and the workload starts
from whatever its storage holds, as after a power cut. A node cut off from the control plane but
not from its clients stops its `stop` workloads anyway, which is what `stop` means. When the node
returns, its epoch has been superseded, so it stops what it still ran and keeps any local data as
an orphan (§4.5 of `02-architecture.md`). What it does not do: move a workload back when its node
returns, which would be the automatic rebalancing §9 refuses; or move a workload whose node is
reachable and failing in some other way, which an operator's drain does (§3).

**Built 2026-10-27, for workloads with no volume or with their volumes in shared classes** (P3.8c;
block and replicated classes are P3.8e and P3.8d, the latter built below). The silent-node sweep,
which marks a node unreachable once its staleness window of three missed heartbeats has passed
(§4.3 of `02-architecture.md`), runs the failover after each of its passes, each workload in a
transaction of its own that locks the workload, then its lease, as a lease request does, then the
placement lock. A workload is moved only while nothing of it is under way: a plan of its target, or
a remake, has applied, and none of its plans is approved, queued, or applying. A plan under way is
the old node's to finish, or an operator's to cancel, and the workload is moved at a sweep after
that; one whose first plan never applied is not moved at all, since its steps may have made part of
it on the old node, and an operator's removal of that node places it again from its target. **What
the new node runs it from, and who puts it there:** its recipe's content and its volumes there were
put on a node by the steps of the plan that made it, which ran on the old node, so the control
plane writes a `remake` plan in the transaction that moves it and approves it itself, under the
grant of the decision it carries out: the recipe pulled, each volume it holds attached where it is,
never made anew nor written by an install, and its runtime created and, if it is to run, started
(Finding 204 of `23-walkthroughs.md`). A volume made with it that has no record was never reported
made, since a node reports a volume before the step that made it completes, and is made where it
lands. A workload made on a node removed as compromised (§3 of `18-operations.md`) is placed and
made again the same way. A failover that finds no node keeps the placement, shows the attempt as
the workload's unplaced status, which no attempt for the intent it is placed for otherwise is, and
is forgotten once the node is heard again. Tested over the API, the lease's term waited out and a
workload whose volume is on the silent node's disk left where it is, saying why, and end to end
with a node's power cut: its agent ends without a word, what it ran ends with it, and the workload
starts on the other node on what the first wrote to the shared store. What it costs: a plan per
workload moved; and a workload with a plan stuck under way on the silent node, or never made, is
not moved until an operator cancels the plan or removes the node.

**Built 2026-10-27, onto a replica** (P3.8d5). A workload declaring `replica_failover: automatic`
reaches a volume on the silent node's disk at the node holding its freshest replica, and the remake
plan there promotes the replica in place of attaching the volume (§2.2 of `06-storage.md`, which
says what becomes of the volume, its snapshots, its other replicas, and the copy left behind); the
effect `workload.failover` records `lost_since`, when that replica received what it holds. Such a
workload's derived `on_lease_expiry` is `stop`, since failover takes no other (Finding 206 of
`23-walkthroughs.md`). Tested over the API and end to end with a node's power cut. What it costs:
the writes after the replica's last snapshot, which the workload declared it accepts, kept on the
old node as a divergent copy and not merged.

---

## 3. Operations: changes that span workloads

> Resolves open question 1 of `01-model.md` and 4 of `02-architecture.md`.

A `Plan` covers exactly one workload. Draining a node touches every workload on it. Upgrading a
recipe touches every workload using it. Approving two hundred plans individually is not a workflow
anyone will use.

```
Operation
  id             OperationID
  kind           drain | evacuate | migrate | rebalance | recipe_rollout | bulk_intent
                 | node_upgrade | template_rollout
  scope          selector or explicit set
  plans          []PlanID          one per affected workload, generated as it proceeds
  policy         OperationPolicy
  status         proposed | approved | running | paused | completed | failed | aborted
  progress       {total, completed, failed, remaining}
```

```
OperationPolicy
  concurrency        int      how many workloads in flight at once
  max_unavailable    int|%    hard ceiling on simultaneous disruption
  on_failure         pause | continue | abort
  window             TimeWindow?    only act inside a maintenance window
```

An Operation is a parent that owns many Plans. It carries one approval, one progress view, and one
pause/resume/abort control. Individual plans still exist, are still computed against their own
`from_intent`, and are still individually audited, nothing is lost, and the human approves the
decision that was actually made ("drain node 7") rather than two hundred restatements of it.

`evacuate` is drain for a node that cannot take part: it is unreachable, so nothing can be copied
from it. It re-places workloads from their intents onto other nodes, which for a workload with
local storage means starting from its latest backup or empty. Its plan names, per workload, which
of those it will be and what data is at risk, and it is never generated automatically for a
`keep_running` workload, whose original may still be running (§4.5 of `02-architecture.md`).

**Bulk changes are Operations too.** "Restart these two hundred workloads" or "set this label on
five hundred" is a `bulk_intent` Operation: one call, one approval, a plan per workload computed
against that workload's own current version, and **per-item results**. A workload that changed
underneath the operation conflicts alone and is reported alone; the rest proceed. All-or-nothing
across hundreds of independently owned workloads would let one person's concurrent edit block
everyone else's change. This answers §12 of `10-api.md` and §10 of `11-surface-web.md` (finding
S18).

**A template rollout is an Operation over grants, not workloads.** Added 2026-10-19, when grant
templates were built (§3.5 and §4 of `08-identity.md`). "Give every Moderator what version 3 adds"
has one approval and a plan per grant issued from an earlier version, each issuing its holder the
difference, attenuated from a grant of whoever approved it. Its items are grants, so its scope is
the template and its `max_unavailable` and `window` say nothing; its per-item results are what
matter, since a holder revoked meanwhile, or one the approver cannot pass the new action to, fails
alone and the rest proceed. It is built with the other kinds, in Phase 3.

`max_unavailable` is the property that makes drain safe. Without it, draining a node means every
workload on it migrating at once, which is a self-inflicted outage. With it, drain is a slow,
observable, pausable process, which is what hardware maintenance actually requires.

Plans are generated **as the operation proceeds**, not all upfront. A plan computed two hours ago
against a cluster that has since changed is a plan that will be `superseded` on arrival (§3.3 of
`01-model.md`).

### 3.1 What a single Plan's steps guarantee

> Finding 2 of `23-walkthroughs.md`.

An `Operation` composes plans. A `Plan` composes **steps**, and creating one workload is already
three durable side effects, a volume with quota, an endpoint allocation, a runtime object. If the
volume is created and the endpoint allocation fails, something exists that nobody declared.

P9 says no operation may leave a workload in a state the model cannot represent. That is the
requirement; these are its mechanics:

- Steps are **ordered**, and the order is in the Plan, visible before it runs.
- Every step is **idempotent**, so retrying a step whose outcome is unknown is always safe.
- A failed plan is **resumable from the last completed step**, not restarted from the beginning.
- Resources created by completed steps are **retained and named in the observation**, never
  orphaned silently, never deleted on the assumption they are unwanted. This is the same treatment
  §4.4 of `02-architecture.md` gives runtime objects an agent finds but did not create.
- The state is one the model can express at every point: the plan is `failed` or `cancelled` with
  `completed` naming the last finished step and the failed step's reason attached (§3.3 of
  `01-model.md`), and the interface shows the workload as **partially applied** from those facts.
  It is the plan's state, held by the control plane, and never an observed state, because the
  agent that writes observations cannot know what the plan intended.

A plan that fails halfway is therefore a plan waiting to continue, which is what makes P9's
"resumable, verifiable, reversible" true of ordinary workload creation and not only of migration.
It continues through the workload's next plan, which is computed from the last intent the workload
reached rather than from the target the failed plan never reached (§3.3 of `01-model.md`): the
steps it completed are planned again and complete as no-ops, the step it failed and every step
after it run, and an install resumes at the action that failed. Declaring the same body again is
how a person asks for that, after fixing whatever made it fail. Added 2026-10-15, Finding 51 of
`23-walkthroughs.md`: before, the next plan was computed from the target and planned nothing.
Until then the node holds what the plans reached (§4.8 of `02-architecture.md`), so a workload
whose first plan failed runs nothing, and its install's saved progress waits on the node for the
next plan (Finding 52 there).

**The guarantee has to reach inside a step, not stop at its edge.**

> Finding 20 of `23-walkthroughs.md`.

"Run the recipe's install" is one Plan step and six install-DSL steps (§4 of `09-recipes.md`).
Resuming from the last completed *Plan* step therefore re-runs a forty-gigabyte SteamCMD fetch that
had already succeeded, and does it over a directory the retention rule above deliberately kept,
which is how `extract` and `template` produce a workload that starts and is subtly wrong.

So the three guarantees are stated at both granularities. Install steps are ordered, individually
idempotent, and resumable from the last completed one, with progress recorded durably per install
rather than per plan. The DSL's verbs are a closed set (§4 of `09-recipes.md`) precisely so that
each one can state its own idempotence, which is a property this design already required and had
only asserted one level up.

---

## 4. Runs: the history of `task` and `scheduled` workloads

> Resolves open question 4 of `01-model.md`.

A `scheduled` workload with a nightly cron produces a run every night. Those are not workloads;
they are executions of one workload, and modelling them as workloads would multiply the object
count by the number of nights and break every quota, grant, and name.

```
Run
  id             RunID
  workload       WorkloadID
  intent         IntentID         which declaration produced it
  trigger        schedule | manual | event | retry
  scheduled_for  Timestamp
  started_at     Timestamp?
  ended_at       Timestamp?
  node           NodeID?          the node it ran on: its workload's when it started
  status         pending | running | succeeded | failed | cancelled | missed
  exit_status    int?
  attempt        int
  streams        []StreamID       this run's own output
  cycle          int?             the power cycle that requested it (§4.1); null otherwise
  occurrences    int              how many occurrences it stands for; above one only for a
                                  missed run recording an interval skipped (§4.3)
  cause          string?          why it ended as it did: exit, timeout, cancelled, stopped,
                                  replaced, concurrency, not_started, skipped, node_lost (§4.2
                                  to §4.4)
```

> Completed 2026-10-18, when runs were designed for building (Finding 98 of `23-walkthroughs.md`).
> `cycle`, `occurrences`, and `cause` are added: without the first a control plane cannot make a
> requested run exactly once, without the second a night skipped while it was down is either
> invisible or a thousand rows, and without the third `failed` and `missed` do not say what anyone
> should do about them. `placement PlacementID` is replaced by `node`, as built: a placement has no
> identifier of its own, being a workload's binding to a node that the next migration replaces, and
> the node is what someone reading Tuesday's run asks.

```
RunPolicy
  concurrency      allow | forbid | replace
  retries          int
  backoff          Duration
  timeout          Duration
  keep_successful  int      how many succeeded runs to retain
  keep_failed      int      failures are retained longer; they are the ones anyone reads
  catch_up         bool     run missed occurrences after downtime, or skip to now
  max_active       int      the most runs active at once: 10 under `allow` unless declared,
                            at most 100; one under `forbid` and `replace` (§4.3, added
                            2026-10-18)
```

`concurrency` and `catch_up` are the two that cause incidents when they are implicit. If last
night's backup is still running when tonight's is due: `forbid` skips it, `replace` kills and
restarts, `allow` runs both, and running two backups against one volume simultaneously is how
people lose data. If the control plane was down for six hours, `catch_up: true` fires six missed
runs at once, which is occasionally right and usually a thundering herd.

Each run holds its own streams (§3.6 of `01-model.md`), so "why did Tuesday's job fail" is
answerable after Wednesday's succeeded.

### 4.1 What makes a run

> Added 2026-10-18, when runs were designed for building (Findings 98 to 100 of
> `23-walkthroughs.md`). The objects above said what a run records, and nothing said who makes
> one, where it runs, what ends it, or in which time zone 03:00 is.

**The control plane makes every run; a node executes only the runs its assignment names.** A run
is made in one of four ways, which is its `trigger`:

- **`schedule`.** The control plane evaluates the target intent's `schedule` in its `timezone`
  (§3.3 of `01-model.md`) for every `scheduled` workload whose power target is `running`, and
  keeps each one's next occurrence. When the time comes it makes the run. The store holds at most
  one scheduled run per workload and occurrence, so two control-plane replicas evaluating the
  same minute make one run and not two.
- **`manual`.** A higher power `cycle` (`workload.restart`, §3.3 of `01-model.md`) on a `task` or
  `scheduled` workload requests one run, made once for that cycle value when the power target
  reaches it. A `task`'s first run is made this way: the plan that creates it brings its power to
  `running` at cycle zero, and cycle zero has no run yet. So a task runs once when it is created
  and once each time someone restarts it, and a restart means the same thing for every
  lifecycle: do it again. For a `scheduled` workload cycle zero requests nothing; its runs come
  from its schedule, and each later cycle is one run now.
- **`retry`.** A failed run with attempts left makes the next attempt, the n-th retry waiting
  `backoff` × 2^(n−1) after the failure ended.
- **`event`.** Nothing makes one yet. It names the runs a later trigger source will make, and it
  stays in the type, because removing a value from a frozen schema costs more than an unused one.

A run is made whether or not the workload is placed. It is pending until a node starts it, and it
reaches a node only in the assignment of the node the workload is placed on (§4.5).

**`timezone` is an IANA zone name, UTC when unset**, checked at declaration against the control
plane's zone database, since the control plane is the component that evaluates it. A local time a
daylight-saving change skips runs at the first instant after the gap; a local time it repeats runs
once, at its first occurrence. What it costs: the control plane carries a zone database, updated
with its machine's, and a zone whose rules changed after that database was built runs an hour off
until the database is updated. The alternative, cron in UTC, puts a backup meant for 03:00 in
Istanbul at 03:00 the whole year and one meant for 03:00 in Berlin at 04:00 or 05:00 by season,
which is the defect the field exists to remove (Finding 100).

> As built, 2026-10-18 (`crates/korpis-core/src/workload/spec/cron.rs`, `spec/runs.rs`,
> `crates/korpis-server/src/zones.rs`). The zone database is the machine's, `/usr/share/zoneinfo`
> or wherever `TZDIR` names, read through a port so the domain reads no files; a control plane
> handed none knows UTC alone. A schedule is five fields as Vixie cron reads them: `n/k` runs from
> `n` to the field's end, Sunday is 0 or 7, and when the day of the month and the day of the week
> are both restricted, neither beginning with `*`, a day matching either is due. One that names
> only days its months never reach, such as the 31st of April, is refused at declaration. A
> completed intent stores its policy as it applies, the defaults written out, and `UTC` for a
> schedule declared without a zone, so that a later default cannot change what a stored intent
> means; a policy or a zone on a workload that cannot use it is refused rather than ignored.

### 4.2 Where a run runs, and what ends it

**A run executes on the node its workload is placed on, as a container instance of its own**,
named for the run, with the workload's volumes, endpoints, and identity (§9.5 of
`08-identity.md`). The workload is placed like any other (§2), **and its reservation is held while
it is placed, not only while a run is running.** That is what guarantees the 03:00 run room on a
node that filled up during the day. What it costs: an idle nightly job holds its reservation all
day. A workload that would rather not declares a limit and no reservation, and its run then starts
on whatever its node has free at that moment, under the overcommit of §5.3.

The node never brings a run's process back the way it revives a crashed persistent workload (§4.6
of `02-architecture.md`): a run ends when its process exits. The node journals `run.started` and
`run.ended` with the exit status, and the control plane believes them only from the node the
workload is placed on. The control plane judges the exit status against the `exit` probe (§5 of
`04-runtimes.md`) of the intent the run was made under (§3.3 of `01-model.md`), zero alone when it
declares none: `succeeded` or `failed`, cause `exit`. The workload's status shows its newest run
(corrected 2026-10-18: this said its observation, but the observation is the node's, and how a
run was judged is the control plane's), and a `task` or `scheduled` workload has converged when
its power target is reached, whether or not a run is active.

**The node enforces `timeout`**, holding the run's start and the timeout from its assignment, so a
run stops at its timeout while the control plane is unreachable: the stop signal, the grace, then
a kill, and `failed` with cause `timeout`. When the workload's lease expires (§4.5 of
`02-architecture.md`), the node does with the process what `on_lease_expiry` says, as for any
workload, and the control plane records the runs it held `failed` with cause `node_lost` and makes
a retry where attempts remain.

> As built, 2026-10-18, the node's half (`crates/korpis-agent/src/reconcile/runs.rs`). The
> workload's own object is never started; a plan's `create`, `start`, and `restart` steps prepare
> its target and start nothing, and `stop` stops its runs, since a run is started as its
> assignment names it and a restart's run is made by the control plane. Each run's object is
> labelled with the run, which the driver folds into the object's identifier, so two runs never
> share one; it is started once and never brought back, and journaled as `run.started` and
> `run.ended` with its exit status and cause (`exit`, `timeout`, or `cancelled`). An object
> that ended is kept while the run is still named, so a node not yet told the run ended never
> runs it twice, and removed once it is not. An agent that restarts takes over each run's object
> by its label and counts its timeout from when its process started, as the driver says. Each
> run's console is written into a stream volume of its own on its node, which §4 of
> `14-streams.md` makes readable. Concurrent runs share the workload's network namespace, which
> holds one listener per port, so a workload declaring endpoints with `concurrency: allow` is
> refused at declaration (Finding 101).

> As built, 2026-10-18, the control plane's half (`crates/korpis-app/src/node/journal/runs.rs`,
> `reported_end` in `crates/korpis-core/src/workload/run.rs`). A report is believed from a node
> the workload is assigned to, for a run of that workload no other node started; any other is
> kept as `journal.rejected` and moves nothing. The node says why it stopped a run, never how the
> run ended: `timeout` is `failed`, `cancelled` is `cancelled`, and anything else is judged by its
> exit status as above, a run with no exit status (one whose object could not be made or started)
> `failed`. A report the run has moved past, a start arriving after a cancel or an end reported
> twice, is believed and changes nothing, so the control plane's own cause stands. The times
> recorded are the node's clock, which is informational, held between when the run could have
> happened (made, or started) and when the report arrived: what that costs is a run reported
> late by a node whose clock runs fast is recorded as ending when the report arrived, not
> earlier. A run is lost when its node no longer holds a current lease on the workload, checked
> on every pass of the run maker, so `node_lost` is recorded within one pass of the lease's
> expiry by the grantor's clock (§4.5 of `02-architecture.md`). The newest run is shown on the
> workload's status (`WorkloadStatus.newest_run`) rather than in the node's observation, since
> the control plane, not the node, holds how it was judged; and a workload that has runs counts
> as converged with its own object stopped while its power target is running.

### 4.3 Concurrency, catching up, and missed runs

A run is **active** while it is pending or running. When a run is due and the workload has an
active one, `concurrency` decides:

- **`forbid`**: the new run is recorded `missed`, cause `concurrency`, and the active one goes on.
- **`replace`**: the active run is cancelled, cause `replaced`, and the node stops it when its
  assignment no longer names it; the new run is made.
- **`allow`**: the new run is made beside it, and the node runs both, up to `max_active`
  running at once: a run due while that many run is recorded `missed`, cause `concurrency`, as
  under `forbid`.

**`allow` is bounded by `max_active`**, ten unless the policy declares another, at most a
hundred, decided 2026-10-18 when the disk quota came to count each run's stream (§4 of
`14-streams.md`). Each run that runs writes a console stream of its own on its node, and a disk
quota is a limit its tenant is shown, so it must hold what the workload can actually use (Rule
K-3 of `00-overview.md`); without a bound on how many run at once, `allow` held a number of
streams nothing bounded, and no quota could count it. It is
counted the same way for every policy: one stream for each run retention keeps and each that
can be active, `keep_successful` plus `keep_failed` plus `max_active`. What it costs: a
workload whose runs must overlap past a hundred is refused, and one that overlaps past its
declared bound misses a run, recorded where anyone reading the runs sees it; and the quota
counts the bound, not what the workload happens to use. With the defaults, three succeeded, ten
failed, and one active, that is fourteen streams at the default bound of 72 MiB each (64 MiB
retained and the segment being written), 1008 MiB of the workload's disk quota for consoles; a
job that needs less declares fewer kept runs and a smaller console, as the nightly backup of §20
of `23-walkthroughs.md` does (figure added 2026-10-19, when that test's node had less disk free
and the workload was refused placement). `forbid` and `replace` are `max_active: 1`, and
declaring another value with either is refused.

The same rule applies to manual and retry runs, so a restart into a `forbid` workload that is
already running is recorded, not lost.

**An occurrence is on time when its run is made within a minute of it.** When the control plane
was down past occurrences, `catch_up` decides what they become:

- **`false`** (skip to now): one run is recorded `missed` for all of them, cause `skipped`,
  `scheduled_for` the first, `occurrences` how many, and the next occurrence is evaluated from
  now.
- **`true`**: each becomes a run with its own `scheduled_for`, oldest first. **Catching up is not
  a concurrency conflict**: under `forbid` they run one after another, each made when the one
  before it ends; under `replace` only the newest runs, since each would replace the one before
  it; under `allow` up to `max_active` start together, which is the herd §4 warns of and what
  `allow` asks for, and the rest wait their turn as under `forbid`.
  At most a hundred are caught up; any older are recorded as one missed run, as under `false`.

**A pending run that never starts** (its node is offline, or it is not placed) is still active.
When its next occurrence comes due, `catch_up: false` records it `missed`, cause `not_started`,
and makes the new one in its place, so a node back after three nights runs tonight's backup and
not the first night's. Under `catch_up: true` it stays pending and the new occurrence waits
behind it, so the same node runs all three, oldest first (Finding 99). `not_started` is the
control plane's account, not the node's: a node that started the run while out of touch journals
that it did when it returns, and the run is then recorded as it ran and ended.

### 4.4 Power, cancellation, and retention

**Power `stopped` suspends the schedule.** No run is made, active runs are cancelled with cause
`stopped`, and the node stops them. Nothing that passes while the workload is stopped is missed,
because nobody wanted it to run; the schedule resumes at the first occurrence after its power is
`running` again. **One run is cancelled** with `CancelRun`, under `workload.stop`, cause
`cancelled`; the node stops it when its assignment no longer names it.

**When a run ends, the control plane keeps the newest `keep_successful` succeeded runs and the
newest `keep_failed` of every other ending** (failed, cancelled, missed), and deletes the rest with
their streams.

The defaults are `concurrency: forbid`, `catch_up: false`, no retries, a `backoff` of ten
seconds, no `timeout`, `keep_successful: 3`, and `keep_failed: 10`. The two §4 says cause
incidents when implicit default to the values that can neither run two backups against one volume
nor fire a herd; an implicit `forbid` at worst skips a night, and the skip is a missed run in the
list anyone reading the runs sees. There is no default timeout because a timeout that kills a
legitimately long first backup is worse than none: a run that hangs under `forbid` makes every
later night a missed run with cause `concurrency`, which is how it is noticed.

Runs are read under `workload.read`, and each run's streams under `workload.console.read`
(§5 of `10-api.md`).

> As built, 2026-10-18 (`crates/korpis-core/src/workload/run/tend.rs`,
> `crates/korpis-app/src/run.rs`). Every ten seconds the control plane takes each workload that
> has runs, or has an active one, in a transaction that holds the workload and its run state, and
> decides from them, the time, and the zone what §4.1 to §4.4 say: one function of what the store
> holds, tested at chosen moments. What it decides is written with an effect for each run:
> `run.made`, `run.missed` with its cause, `run.cancelled`. Two control planes take a workload in
> turn, and the store makes one run per occurrence and per cycle besides, so two passes over the
> same minute make one run. A retry is made when the newest run failed with attempts left and its
> backoff has passed, so a failure the node reports and a node lost are retried alike; the
> retry's `scheduled_for` is when it came due. Under `catch_up: true` with `replace`, the caught-up
> occurrences but the newest are one missed run with cause `replaced`. Retention runs on every
> pass (corrected 2026-10-18: it ran only after a change the pass made, so a run its node reported
> ended, or one cancelled, was kept past retention until something else changed). A workload
> retired with an active run has it cancelled, cause `stopped`, as one whose power is stopped.
> Each change announces the workload, so its node and its watchers hear of it. A run retention
> deletes takes its stream with it: the control plane ends the stream, whose offloaded segments
> are then kept no longer than the operator's floor, and the node deletes the stream's volume
> once the assignment no longer lists the run among those the control plane holds (§4 of
> `14-streams.md`).

### 4.5 What a node is told of a run

**The assignment carries the runs the node is to execute** (§4.8 of `02-architecture.md`): each
one's identifier, attempt, trigger, `scheduled_for`, and timeout. The node starts each named run it
has not started, stops each one it started that is no longer named, and starts nothing for a
`task` or `scheduled` workload that names none, which is how a workload with no active run sits
on its node holding its volumes and nothing else. Each run's console is a stream of its own
(§3.6 of `01-model.md`, §4 of `14-streams.md`). A run a node presents after a restore that the
restored store lacks is recorded as recovered, with what the assignment named, like the rest of
the assignment (§8 of `03-state.md`).

> As built, 2026-10-18, the assignment's half (`crates/korpis-core/src/workload/run.rs`,
> `named`). Under `allow` the assignment names every active run. Under `forbid` or `replace` it
> names the running one, or else the oldest pending one by its occurrence, so runs queued behind
> one another, caught up or waiting behind one never started, reach the node one at a time and
> the node needs no policy of its own to keep them apart. The timeout named is the target's
> policy's, which is what the node runs.

---

## 5. Quota, overcommit, and honesty

> Resolves open questions 5 and 7 of `01-model.md`.

### 5.1 Quotas are scoped

A hosting provider sells "4 GB in the EU". Capacity in one region is not interchangeable with
capacity in another, so a single global bound cannot express what is being sold.

```
QuotaSet
  entries []QuotaEntry

QuotaEntry
  selector     LabelSelector?     // null = applies to everything
  nodes        LabelSelector?     // null = counted from declaration; else where it is placed
  reservations Resources          // guaranteed; sums against the parent
  limits       Resources          // ceiling; may exceed the parent, by declared ratio
  version      int                // participates in the admission CAS, see below
```

Admission requires an intent to fit within **every** matching entry. A tenant can hold a global cap
of 32 GB and a per-region cap of 4 GB simultaneously, and both are enforced. Composable, and it
replaces the flat `Quota` struct of `01-model.md` §3.1.

The dimensions are those listed in §3.1 of `01-model.md`, plus **dedicated IPv4 addresses**, a
count that is exclusive and never overcommitted, and, added 2026-10-15, **node ports**, held by
`node_port` and `node_bound` endpoints, likewise exclusive (§3 of `07-networking.md`). IPv6
addresses are not a dimension: they are abundant, and counting them would make the scarce resource
and the plentiful one look alike (§11 of `07-networking.md`, question 4).

#### Inheritance: reservations sum, limits may oversell

> Finding 6 of `23-walkthroughs.md`. This is the question Bet 4 actually turns on.

A reseller holds 32 Gi, sells 16 Gi to one customer and 16 Gi to another, and wants to sell 16 Gi
to a third, because customers use a fraction of what they buy, and overselling is not an abuse of
this market; it is the ordinary commercial practice of every host Korpis is courting.

Counting a child's quota against the parent **by allocation** forbids that. Counting it **by
usage** means quota is not a guarantee, and P4 breaks the first time three customers get busy at
once.

So the same distinction §5.2 draws for a workload's resources is drawn for quota:

| | Against the parent | Meaning |
|---|---|---|
| `reservations` | **sum, never oversubscribed** | what the reseller has actually guaranteed |
| `limits` | may exceed, by a declared ratio | what the reseller has sold |

A reseller sells 16 Gi limits backed by 4 Gi reservations and can see both totals, promised versus
guaranteed, which is the spreadsheet they are keeping today, made into a first-class number. The
overcommit ratio is per-dimension and memory defaults to off, exactly as §5.3 requires; a parent
that has not enabled memory overcommit cannot oversell memory at all.

Bet 4 survives, and it survives because the mechanism already existed one layer down and had simply
never been applied to quota.

> Corrected 2026-10-22 (Finding 139 of `23-walkthroughs.md`). The sets below were checked to sum
> within the set above, but admission at an organization's entry counted only what the workloads
> beneath it hold, so a reseller's own workloads could take what it had guaranteed a sub-tenant,
> who was then refused below its guarantee: "never oversubscribed" held for what was given and
> not for what was run. A guarantee is now held where it is evaluated. An organization's entry
> counts each child organization's entry of its shape as the larger of what the child was given
> and what it holds, beside what the organization's own workloads hold; a workload within the
> guarantee of the child counting it is not refused by the entry above that child, whatever the
> parent holds; and giving a child more is refused when the parent's own holdings and everything
> it promised would exceed its bound (below, "Setting a quota set"). **What evaluates this, where,
> and with what information:** the control plane, at admission and at a set's change, from the
> entries it locks and the entries of the organizations directly beneath their owners, read under
> the lock of the parent's entry, which every change to what a child's entry counts or was given
> also takes. What it costs: promising is reserving, so a reseller's own servers run only in what
> it has not promised; and a host that lowers a reseller beneath what it guaranteed and runs leaves
> the reseller's own servers unable to grow until it holds less, while its children keep theirs.

#### Admission holds what it checks

> Finding 1 of `23-walkthroughs.md`.

`10-api.md` §6 protects a mutation with compare-and-swap on `expected_version`, of the *workload*.
Quota is a property of the organization, so that CAS protects the wrong object: two concurrent
creates each observe 24 Gi of 32 Gi used, each pass admission, and both commit.

**Quota consumption is written in the same transaction as the intent, against the matching
`QuotaEntry` rows, whose `version` participates in the compare-and-swap.** The contended object is
the quota entry, not the workload. A losing writer gets `CONFLICT` and re-admits against the new
figure.

A quota that is checked without being held is advisory, and P4 forbids displaying an advisory limit
as a limit.

#### What admission counts, and against what

> Added 2026-10-13, from the implementation (`crates/korpis-core/src/tenancy/quota.rs`,
> `crates/korpis-app/src/workload/admit.rs`). The paragraphs above say that admission holds the
> matching entries; they did not say which entries match, what a workload holds, or what a change
> that shrinks a workload owes. Each answer names what evaluates it and with what information.

- **Which entries.** The entries of the workload's project and of its organization and every
  organization above it: an organization's entry bounds everything below it, sub-organizations
  included, which is how a reseller's 32 Gi bounds its customers' customers. The control plane
  evaluates this at declaration, from the tenancy tree it holds. Each organization's entry counts
  what it promised its children beside what it holds, and admits a workload within its child's
  guarantee (corrected 2026-10-22, above).
- **What a selector matches: the workload's labels.** They are the only description of the
  workload admission has; where it will run is decided later, by placement. Question 6 below is
  what that leaves open.
- **What a workload holds: per dimension, the larger of its head and its target.** The head may be
  approved at any moment and the target is what the kernel grants now, so holding either alone
  would let a workload awaiting approval, or one being shrunk, hold less than it may use. A limit
  left unset is the reservation (§5.2); an I/O ceiling left unset is unbounded, which no bounded
  entry admits. Disk includes every volume the workload creates and the most its console stream
  can keep (§5 of `14-streams.md`), and placement reserves the same on the node, since a stream's
  segments are written there.
- **A refusal names the set to whoever may read it.** Added 2026-10-22 (Finding 140 of
  `23-walkthroughs.md`). It names the set by its owner, `organization org_…` or `project prj_…`,
  with its bound and what would be held, to a caller who may read that owner (`org.read` or
  `project.read`, which show the set); any other caller, a customer refused by its reseller's
  set, is told only the dimension and the side, since the reseller's bound and what everything
  beneath it holds are the reseller's (§8 of `10-api.md`). Both are `QUOTA_EXCEEDED` with the
  dimension as the field. A node's refusal at placement, which whoever may read the node is shown
  (§8 of `08-identity.md`), names the set and the dimension and no number, since a tenant may own
  the node. The first build told every caller the entry's text, the ancestor's numbers included,
  under an identifier no reply carries.
- **Only growth is refused.** A declaration that holds no more than the workload already did, in a
  dimension, passes even where the entry is exceeded, so lowering a bound never strands a workload
  that then cannot be edited, not even to shrink it.
- **Holdings are computed from the intents, under the entries' locks**, never kept as a running
  counter that could drift from them; the entries' versions are bumped in the admitting
  transaction as the record that they were consumed. Entries are locked in identifier order,
  after the workload's own row and before the placement lock, so two transactions never wait on
  each other in a cycle. A dry run
  performs admission and rolls back, so `korpis plan` shows a quota refusal before anything is
  sent for real.

What it costs: every declaration reads and decodes every workload under the entries that count it,
under a lock that serializes the declarations those entries count. For a project that is a handful
of rows. For an entry on a root organization at the comfortable scale of §9 of `03-state.md`, it is
up to 50,000 bodies per declaration, which is too slow to hold a lock over. The design that
survives that is a held total per entry, written in the same transaction as every intent and
recomputed from the intents by a reconciliation, so a drift between them is found rather than
trusted; it is not built yet, and question 7 below says when it must be.

> Held totals as built, 2026-10-19 (`crates/korpis-app/src/workload/hold.rs`, question 7 below).
> The holdings are still computed from the intents, but one workload at a time: each entry keeps
> the totals of what the workloads it counts hold, and each workload keeps what it was last
> counted as holding and by which entries. Every change to what a workload holds (a declaration,
> its target moving at approval, cancellation, or a restore's recovery, and its retirement)
> recounts that one workload in its own transaction: what it was counted as holding comes out of
> the entries that counted it, exactly, and what it holds now goes into those that count it now.
> Admission reads an entry's totals, less what the entry already counts of the workload being
> declared, so a declaration reads two intents and one row per entry, whatever the entry counts.
> An entry is reckoned from its workloads' holdings the first time it is used, under its lock. A
> reconciliation runs at start, before anything is served, and hourly: it recounts every
> workload from its intents, reckons any entry not yet used, and checks every other entry's
> totals against what its workloads are counted as holding; each difference it corrects is a
> `quota.drift` effect, since only a change that skipped its recount makes one. What it costs:
> the hourly pass reads every workload's intents, one short transaction each, and takes each
> entry's lock briefly per workload under it; an entry made while a workload under it is being
> declared can miss that one workload until the next pass, which then says so.

#### Setting a quota set

> Added 2026-10-19, when quota sets became settable (`crates/korpis-app/src/tenancy/quota.rs`).

An organization's set is its parent's decision, so `SetQuota` replaces it under `org.quota.write`
over the parent (§5 of `10-api.md`): no tenant raises its own. A root organization holds no set:
what it holds is the installation's, and Korpis limits no host (P10). A project's set is a
sub-limit within its organization's and is set under `org.quota.write` over the organization.

The set is checked in one transaction holding the locks of every set it is checked against: the
parent's, the siblings', the organization's children's, and its projects'. Entries are compared
**by shape**, the pair of their workload selector and their node selector, because entries of one
shape at two levels are the same promise made at each. Per shape and dimension: the reservations
given to the organization and its siblings sum within the parent's, never oversubscribed, and
together with what the parent's own workloads hold, within the parent's bound unless that was
already so before the change (Finding 139 of `23-walkthroughs.md`); their
limits sum within the parent's times the overcommit percent the parent was permitted, none where
it was permitted none, and never for dedicated IPv4 or node ports; what the organization gives its
own children still fits what it is given now, so lowering a reseller below what it gave is
refused (`quota.overallocated`) and the children are lowered first; and each project's entry fits
the organization's entry of its shape. A shape the parent has no entry of, or a dimension its
entry leaves unbounded, holds the children to nothing at this level; admission still counts them
against every entry above. Within one set, each shape appears once and no reservation exceeds the
limit beside it. A bound of zero is a bound, which allows none of a dimension: the wire carries
bounds with explicit presence (`QuotaBounds`), since `Resources` could not tell zero from absent.
The shaping applied at an exhausted egress budget is refused until it is built (Phase 2's
bandwidth shaping) rather than accepted and not applied (Rule K-3). Built 2026-10-21 (P2.8e): it
is accepted now, with the budget it applies to ("The egress budget, as designed", below).

An entry whose shape the new set keeps keeps its identity and the totals of what it counts, and
takes the new bounds as its next version; an entry of a new shape is reckoned at its first use;
an entry whose shape is gone is removed, and what counted it no longer does. Lowering a bound
below what is already held is allowed: admission refuses only growth (above), so a workload is
never stranded. What it costs: a change first locks the rows of the organization above it and of
its own, top down, and only then reads any set; changes to sets beneath one organization are
therefore serialized, which is what stops two siblings from each being given the last of their
parent's reservations at once, and a project or workload made in the organization meanwhile does
not wait, since the lock leaves the row's key alone.

> Corrected 2026-10-19, the day it was built (Finding 107 of `23-walkthroughs.md`). The first build
> locked only the entries of the sets it read. Under read-committed isolation a change that waited
> on the parent's entries then read its siblings' sets with a snapshot taken before the other
> change committed, so a sibling's entries that the other change had just made were not counted,
> and two siblings could each be given the last 20 GiB of 32. Locking existing rows cannot cover
> rows not yet made; the organization rows above them can.

#### Where a workload runs

> Added 2026-10-19, settling question 6 below, when quota sets were built for Phase 2.

"4 GB in the EU" is capacity in a place, and where a workload runs is decided by placement, after
admission. So an entry may name **the nodes it counts**, by a selector over node labels
(`nodes`). Such an entry counts a workload its `selector` matches while that workload is placed on
a node its `nodes` selector matches, and it counts what the placement runs: the holding of the
target bound there, which is what the kernel grants on that node. It is a filter check of
placement (§2.1): a node is refused while what the workload would run there exceeds an entry
counting that node, with the entry, the dimension, and the figures as the reason, so a workload
that may run anywhere goes where there is room and one that must run in the EU waits, unplaced,
saying why. Only growth is refused, as at declaration: a workload an entry already counts is
checked against what it ran there before. An operator placing a workload by hand is refused by it
too, since it is the tenancy's quota and not the operator's own decision about a node. An entry
that names no nodes counts the workload from its declaration, wherever it runs, as above.

Every change to a placement (a bind, a following in place, an unbind, a node lost, a restore's
recovery) recounts where that one workload is counted, at its end and under the placement lock,
which each of them holds: what the placement was counted as holding comes out of the entries that
counted it, exactly, and what it runs now goes into the entries counting the node it is on now.
These entries' totals therefore move only under the placement lock, and they are never locked by
admission at declaration; the order everywhere is the workload's row, then the entries counted
from declaration, then the placement lock. Node labels are set at enrollment and do not change, so
a node never moves between regions under a placement. The reconciliation checks these entries'
totals and each placement's count as it checks the rest.

What it costs: the binding transaction reads the entries of the workload's project and of every
organization above it that name nodes, and writes the totals of those counting the old node and
the new one, all under a lock every placement already takes. A workload declared within its
global entries may still find no node with room in its region, and is then unplaced with the
reason rather than refused at declaration; the declaration cannot know where it will run.
Removing a node now locks the workloads on it before the placement lock, as every placement does,
which it did not: a workload placed on the node between the two is a conflict, and the removal is
asked again.

#### Quota holds the enforced value, not the declared one

> Finding 17 of `23-walkthroughs.md`.

The two are normally identical, which is why it took a trace to notice they are not always. §6 of
`13-surface-cli.md` lets a declared set drift on purpose: someone raises a memory limit through the
panel at 03:00 to stop an OOM loop, the set is marked drifted, and the change stands because
`advisory` is the default and punishing people for fixing outages is how a tool gets worked around.

For as long as that drift lasts, the file says 4 Gi and the cgroup says 8 Gi.

**Quota is held against what the kernel is actually granting.** Quota is a statement about
capacity, capacity is the thing being sold, and a reseller's headroom is consumed by the enforced
number whatever a file elsewhere says. Holding the declared value instead would let a tenant exceed
a guarantee by drifting, which is a quota bypass reachable from the panel.

The consequence lands where an operator can act on it: a drifted set reports its **quota delta**
alongside the drift in `korpis status`, so the 03:00 fix that quietly consumed a reseller's
headroom is visible the same morning instead of at the end of the month.

#### The egress budget, as designed

> Designed 2026-10-21, with shaping (§7 of `07-networking.md`).

**A budget is a bound on what was used, so it is enforced after use, by shaping.** Every other
dimension bounds what is held, at admission. `egress` bounds the bytes the workloads an entry counts
send beyond their links over a calendar month of UTC days (§3.1 of `01-model.md`), which is known
only once it is metered. So an entry's limits carry `egress_bytes_per_month`, and nothing is refused
at admission for it. Every minute the control plane sums, for each entry with one, the egress its
workloads' records hold for the month as an invoice sums them (§2 of `15-observability.md`), and an
entry whose sum reaches its budget is exhausted until the month ends. Each workload an exhausted
entry counts is told so in its next assignment with the entry's `egress_exhausted`: a floor rate
its uplink class and its link are held to instead of its limit, or, with `cut_off`, nothing leaving
its link for beyond the node, replies included, since a served workload's egress is almost all
replies. A workload counted by several exhausted entries is held to the strictest: a cut-off before
any floor, the lowest floor before a higher. A budget raised above what was used lifts the state at
the next pass. An entry
with a budget and no `egress_exhausted` is refused, since a budget that changes nothing when it is
spent is a number displayed and not enforced (Rule K-3). At the month's turn the state lifts at
each workload's next assignment.

What it costs, and what it does not do. A budget is overrun by what its workloads send before it is
known to be spent: up to an interval of samples, an hour by default, at their ceilings, and a node
that cannot reach the control plane keeps sending at its last state until it can. A host sets the
floor knowing that, and sets a shorter interval where an overrun costs more than its rows (question
1 of §7 of `15-observability.md`). The bytes are those that left the workload's link for anything
but another workload's on its node, so traffic to the node's own ingress proxy counts, being sent to
the world on the workload's behalf, and traffic between two workloads of a node does not.

> Built 2026-10-21 (P2.8e of the build plan). An entry carries `egress_bytes_per_month` beside
> `egress_exhausted`, a floor or a cut-off; the two are set together or not at all, and a floor
> of zero is refused as a cut-off unnamed. The budget is not a bound admission checks, so it lives
> beside the bounds and not among them. Every minute, with metering's own pass, the control plane
> reads every workload placed now or with egress recorded this month, on each node, with the
> bytes its records hold there as an invoice sums them, and each entry with a budget sums those
> it counts: its owner's, selected by its labels, and, for an entry that names nodes, sent from
> those nodes. What each spent is kept apart from the entry (`quota_egress`), so reckoning never
> waits on admission, which locks the entry, and `GetOrganization` and `korpis quota show` say
> it with when it was reckoned. Each workload a spent entry counts is held where it is placed, to
> the strictest of its spent entries' exhaustions, in a row of its own (`workload_egress`) whose
> trigger announces the workload, so its node is told in its next assignment; a row is read only
> for the month it was found in, so the turn of the month lifts every hold at once, and the next
> pass removes the rows. A budget raised or removed lets its workloads go at the next pass. The
> agent hands the hold to the helper, which holds a floor on the workload's link and its class on
> the uplinks, and a cut-off in the host table (§7 of `07-networking.md`). Decided as it was built:
> a node that gives networks and cannot shape holds a floor as a cut-off, the stricter of the two,
> since holding nothing would display a spent budget the kernel does not hold (Rule K-3), and says
> so in its log; what it costs is that a host's floor is a cut-off on such a node until `tc` is
> installed there. The API test sets a budget, is refused one that names nothing to do, spends it
> with two samples, finds the workload's next assignment holding it to its floor and the entry
> saying what it spent, and raises the budget to find the workload let go.
>
> Corrected the same day (Finding 126 of `23-walkthroughs.md`, from a security review of the
> commit that built it). The reckoning chose the workloads an entry counts by their owner and
> labels as they stood at each pass, so a holder able to relabel a held workload out of a spent
> entry's selector would have lifted its hold and taken what it sent out of the entry's sum,
> lifting every other hold the entry made. Nothing changes a workload's labels, project, or
> organization after it is made, so no one could; but nothing said that either, and a label
> edited later would have opened it. An entry now counts a workload it has selected once in a
> month for the rest of that month (`quota_egress_selected`), whatever the workload says of itself
> later; which nodes an entry names is the operator's to change, and is still read for each
> record. What it costs: a row per entry and workload it selects in a month, forgotten when the
> month ends, and a parent who narrows an entry's selector mid-month sees its budget keep counting
> what it counted until the month turns.
>
> Corrected 2026-10-26 (Finding 181 of `23-walkthroughs.md`, found reading the agent while
> designing rules that name workloads). The agent handed the hold to the helper only when it made
> the workload's network, which it did when it created the workload's container and when what its
> dependencies reach changed. A workload running when its budget was spent went on at its limit
> until it was next created, and one let go went on held: the API test saw the assignment say it,
> and no test saw the node do it. The agent now makes a running workload's network again whenever
> what it would ask the helper differs from what it last asked, the hold among it, and once after
> it starts, so a hold is in the kernel within one pass of its assignment; the agent's test holds a
> running workload and lets it go without creating it again. What it costs: a running workload's
> network made again on each change, which rewrites its chains and classes; its connections
> survive, what is metered goes on into named counters a rewritten chain keeps counting into, and
> the counts of its own rows, under `dependency` and `policy`, start again.

### 5.2 Reservation and limit are different numbers

```
ResourceSpec
  cpu       {reservation, limit, isolation: shared | dedicated_cores}
  memory    {reservation, limit}
  disk      {reservation, limit}
  io        {limit, share}           ceilings in bytes/s and ops/s; a proportional share
  bandwidth {egress: {reservation, limit}, ingress: {limit}}
  pids      {limit}                  ceiling only, nobody reserves the right to fork
```

> Second external review, findings S38, S47, and S48. Recorded in §17 of `23-walkthroughs.md`.
> `iops` had a reservation, and cgroup v2 has no I/O floor: `io.max` is a ceiling and `io.weight` is
> a share of contended capacity, so a "guaranteed" IOPS figure was a displayed number nothing could
> hold. `bandwidth` had a reservation in both directions, and no node can guarantee what arrives at
> it (§7 of `07-networking.md`). And the `guaranteed` class was sold as "dedicated vCPU" with
> nothing that dedicated a core. Each is now what the kernel can actually do.

`pids` has no reservation because process slots are not a capacity anyone sells; it exists solely
as a ceiling, and it is the one dimension with no graceful degradation on the way to it. A tenant
process that forks without bound exhausts the node's PID space and takes every other tenant on that
node down with it, the cheapest denial of service available to anyone with a shell. It is enforced
by cgroup v2 `pids.max`, it has a default, and there is no configuration in which it is absent.
The default is 4,096 processes and threads, applied by the driver's `Prepare` when the intent sets
none, and a driver refuses a spec without one (§6.1 of `02-architecture.md`), so the default does
not depend on the supervisor remembering it. (Added 2026-10-13, from the implementation: the
sentence said there was a default and no document said what it was or what applied it.)

- **Reservation** is guaranteed. It is subtracted from `Node.allocatable` and is never
  oversubscribed by anyone.
- **Limit** is the ceiling. The gap between reservation and limit is burst, and burst is what may
  be contended.

This yields three service classes that map exactly onto what hosting providers already sell, and a
fourth property that is sold separately because it costs separately:

| Class | Condition | Sold as |
|---|---|---|
| `guaranteed` | reservation == limit | "guaranteed CPU time", "dedicated RAM" |
| `burstable` | reservation < limit | "shared vCPU", "fair-share" |
| `best_effort` | reservation == 0 | free tier, batch, scavenger capacity |
| **`dedicated_cores`** | `cpu.isolation: dedicated_cores` | **"dedicated cores"**: whole physical cores pinned to the workload, with no other tenant on their SMT siblings |

`guaranteed` CPU is CPU **time**: the workload always gets its share, on cores it shares with
others. That is real and it is what `cpu.max` and `cpu.weight` enforce, and it is not a dedicated
core, so it is not sold as one. `dedicated_cores` is enforced by `cpuset` and by the scheduler
accounting whole physical cores, siblings included, so a workload asking for four dedicated cores
consumes four cores and their siblings from `allocatable`. It is the only configuration that defends
against the cross-core side channels of §5 of `17-security.md`, in any tier, and its density cost is
stated in the plan that requests it.

> Corrected 2026-10-25 (Finding 167 of `23-walkthroughs.md`): the paragraph above says what
> enforces dedicated cores, and nothing did. The schema took `cpu.isolation: dedicated_cores` from
> its first version, while no driver set a `cpuset`, the scheduler accounted no whole cores, and
> no phase of `20-roadmap.md` named the work; a workload asking for them was placed on shared
> cores and stored as asking for them. They are now homed in Phase 3 (§4 there). Until they are
> built, a declaration asking for them is refused, naming that phase, and placement treats them as
> a limit like any other: a node is a candidate only where its driver declares `dedicated_cores`
> enforced (§4.1 of `04-runtimes.md`), which no driver of this release does.
>
> Built 2026-10-25: both drivers declare `dedicated_cores` held where their objects are all under
> `shared` and `dedicated` and the kernel makes the partitions or the host shares the cores, and
> a declaration asking for them is accepted. Placement still treats them as a limit, so a node
> whose drivers do not declare them held is never chosen for them.

#### Dedicated cores, as they are held

Designed 2026-10-25, as Phase 3 builds them (Finding 167 of `23-walkthroughs.md`).

**A workload asks for a whole number of physical cores.** Its CPU reservation equals its limit and
is a multiple of 1,000 millicores, each 1,000 one core, and a declaration otherwise is refused. A
core is what the kernel's topology says shares one core's execution units
(`/sys/devices/system/cpu/cpuN/topology/core_cpus_list`): two logical CPUs on a machine whose SMT
is on. The workload is given every logical CPU of each core it holds, so nothing else shares a
sibling of its cores, and each core counts its logical CPUs, at 1,000 millicores each, against the
node's allocatable CPU: four dedicated cores on a machine with two threads a core take 8,000
millicores there, which is the density cost a plan that asks for them states. Quota counts what the
declaration asks, 1,000 millicores a core, since admission knows no node (§5.1). Its limit is not a
quota: an object holding cores is given none, its cores its bound (§4.2 of `04-runtimes.md`), so
wherever a declaration's CPU is shown, one holding dedicated cores shows its cores and never a
limit in millicores that no kernel holds (Rule K-3).

**A node's cores are read by its agent, and all but the first are offered.** The agent reads the
topology when it starts and reports each core it offers, its logical CPUs and their NUMA node
(`Node.cores`, §3.4 of `01-model.md`). The core holding CPU 0 is never offered, so the shared
workloads, the host, and the node's own agent always keep at least one; an operator names the
cores offered with `--dedicated-cores` (§2 of `18-operations.md`), and a core with a logical CPU
offline is not offered.

**The scheduler chooses the cores when it binds, and the placement holds them**, as it does devices
(§6.3): those a placement following its target in place already holds first, then cores of the NUMA
node with the most free before another's, so a workload's cores and its memory stay on one node
where they fit, then the lowest ids. The filter keeps a node only where one driver offering the
workload's tier declares `dedicated_cores` enforced (§4.1 of `04-runtimes.md`) and enough offered
cores are free. The placement records the cores (`Placement.cores`) and reserves what they take on
that node, their logical CPUs' millicores, rather than the reservation declared; a node's held
cores are derived from its placements, and freeing and restoring follow placements, never a
separate ledger.

**Nothing else runs on a held core.** A driver puts an object that holds cores in a cgroup of its
own under `<parent>/dedicated`, its `cpuset` the cores' logical CPUs and their NUMA nodes' memory,
and every other object under `<parent>/shared`, whose `cpuset` the node's helper keeps at the
node's online CPUs less those held, told by the agent which its placements hold and where each
driver's objects live, which a driver declares (`cgroup_parent`, §4.1 of `04-runtimes.md`) and the
helper keeps only where its `--cores-cgroup` allows (§3 of `18-operations.md`), writing it before
an object holding cores is created and after one is destroyed; a shared object's tasks leave a core
when it is held, the kernel moving them. That keeps every other tenant off. To keep the host off
too, the holding object's cgroup is made a cpuset partition of its own (`cpuset.cpus.partition`, a
remote partition of cgroup v2, which kernels from about Linux 6.7 have), the helper writing the
held CPUs into `cpuset.cpus.exclusive` of each cgroup above it first, as the kernel requires: no
process outside the partition, the host's agent, containerd, and services included, can run on its
CPUs while it is held (§5 of `research/evidence.md`). That is the default. An operator who would
rather the host keep every CPU passes `--dedicated-cores-host shared` to the node's drivers, and
each one's declaration of `dedicated_cores` then says that other tenants are kept off and the host
is not (§4.1 of `04-runtimes.md`), which a plan that asks for them shows; without it, a node whose
kernel has no remote partitions offers no dedicated cores, preflight saying why.

What it costs and does not do. A partition takes its CPUs from every process outside it, the
host's own services included, for as long as it is held: that is what makes a core the workload's,
and a host whose services need those CPUs runs fewer dedicated cores. Per-CPU kernel threads and
interrupts still run on a held core: the kernel's own housekeeping, and the interrupts the
operator does not steer elsewhere with `irqaffinity`, which preflight names where a node offers
dedicated cores. `cpuset.mems` keeps the workload's pages on its cores' NUMA nodes, not the
kernel's allocations made for it. A core is given whole, so on a machine with SMT on a dedicated
workload pays for both threads of each core whether it uses them or not. The meter records each
held core for the time it is held, whatever the workload does on it (§2 of `15-observability.md`).
And the partition path is not run end to end on the machine this project is built on, whose other
services it would take CPUs from: what is tested there is the opt-out, tenants kept off by the
shared `cpuset`, and the partition's files written in the order the kernel asks, against a tree of
the test's own (§4 of `20-roadmap.md`). End to end, a workload declared asking for a core is placed
on a node offering the machine's last core, runs on that core's CPUs alone while its node's
`shared` cgroup leaves them, keeps a second off the node until it is retired, and frees them; and
both drivers pass the conformance cases for dedicated cores (§8 of `04-runtimes.md`), each with the
host sharing the cores.

**I/O is ceilings and shares, never a reservation.** `io.limit` sets `io.max` (bytes and operations
per second, read and write). `io.share` sets `io.weight`, which is a proportional share of the
device under contention and does nothing at all unless the node runs the `io.cost` controller; a
node without it does not offer shares, and preflight says what to configure (§2 of
`18-operations.md`). Both are offered only where the storage class declares them enforceable,
because the cgroup cannot attribute I/O that the filesystem issues from its own threads, which on
ZFS is most writes (§3 of `06-storage.md`).

### 5.3 Overcommit is per-dimension, and never applies to reservations

| Dimension | Overcommit | Why |
|---|---|---|
| CPU | yes, configurable ratio | time-shared; contention degrades gradually |
| Dedicated cores | **no** | a pinned core is exclusive or it is not dedicated |
| Memory | **discouraged, off by default** | there is no gradual degradation; there is the OOM killer |
| Disk capacity | yes (thin provisioning) | but see below |
| Disk I/O | not applicable | there are no reservations to overcommit, only ceilings and shares |
| Egress bandwidth | yes, in the burst region | time-shared |
| Devices (GPU, passthrough) | no | exclusive by nature |
| Dedicated IPv4 | no | exclusive by nature |

Overcommit applies **only to the burst region**. `allocatable` for reservations is always physical
capacity divided by one. A `guaranteed` workload is never oversubscribed, whatever the node's
ratio, otherwise the word means nothing.

Thin-provisioned disk carries a specific obligation under P4: a tenant told they have 100 GB must
actually be able to write 100 GB. So a node whose thin provisioning is approaching physical
exhaustion raises pressure, stops accepting placements, and alerts **before** any tenant hits a
limit they were never told about. Selling capacity that does not exist and discovering it when a
customer's write fails is the failure mode this exists to prevent.

Every one of these is kernel-enforced and metered, or it is not offered (Rule K-3). "Offered" is per
node and per storage class, from the driver's `enforcement` declarations (§4.1 of
`04-runtimes.md`): a dimension a node cannot enforce is not offered on that node, rather than being
displayed there and quietly not held.

---

## 6. Devices

> Resolves open question 1 of `04-runtimes.md`.

A GPU is three things at once (a resource dimension, a placement constraint, and a driver
capability) which is why it does not fit as a scalar like CPU. Generalizing beyond GPUs produces a
cleaner model that also covers hardware hosting operators genuinely need: capture cards, licensing
dongles, TPMs, high-precision clocks.

```
Node.devices[]
  id            DeviceID
  kind          gpu | usb | pci | tpm | ...
  model         string
  attributes    map[str]str      vram, compute capability, bus
  modes         []DeviceMode     passthrough | mediated | timeslice
  allocated_to  []WorkloadID     one entry for passthrough, many for mediated/timeslice
```

```
Intent.devices[]
  kind, count, selector, mode
```

Devices are **discrete named objects**, not a scalar count. They participate in filtering (does
this node have one), in quota (how many may this tenant hold), and in driver capability checks
(does this driver support this mode). Passthrough is exclusive; mediated and timeslice are shared
with declared partitioning.

> Corrected 2026-10-25, when devices were designed in full (Finding 168 of `23-walkthroughs.md`):
> the sketch above kept `allocated_to` on the node's device, a second record of what placements
> hold, and §6.3 derives it from placements instead, as `allocated` is; its `kind` is §6.1's
> `class`, its `model` the device's CDI name and annotations, and its modes a device's own only
> for sharing, since whether a mode can be given at all is the driver's (§4.1 of
> `04-runtimes.md`), which declared modes for GPUs alone.

### 6.1 How a node learns its devices

**A node's devices are the ones the Container Device Interface describes, read by its agent**
(designed 2026-10-25, Phase 3). The Container Device Interface (CDI) is the format accelerator
vendors already ship: their toolkits write a JSON or YAML file under `/etc/cdi` or `/var/run/cdi`
naming each device as `vendor.com/class=name`, with the edits a runtime makes to a container's OCI
spec to give it one: device nodes, mounts, environment, groups, and hooks (§5 of
`research/evidence.md`). NVIDIA's toolkit generates such a file for a machine's GPUs and their
partitions, so a host who installs the vendor's driver has what Korpis reads, and nothing of
Korpis's knows a vendor. The agent reads the spec directories when it starts and every half minute
after, so a file the toolkit rewrites reaches the node's facts by the next heartbeat, reports each
device in its facts (`Node.devices`, §3.4 of `01-model.md`), and never writes them. It orders them
as CDI's reference implementation does: `/etc/cdi`, then `/var/run/cdi`, a device of a later
directory replacing one of the same name in an earlier, a name two files of one directory describe
offered by neither, and a file with a field the specification does not define not read at all, so
no edit a vendor wrote is silently dropped (§5 of `research/evidence.md`).
containerd reads the same directories only in its CRI plugin, which the `oci` driver does not use,
so the driver applies a device's edits itself (§4.2 of `04-runtimes.md`).

Each device reported says four things:

- **its id**, the CDI name, unique on its node;
- **its class**, which CDI does not have: the kind's name where it names a class Korpis knows
  (`nvidia.com/gpu` is a `gpu`), otherwise the class the operator maps the kind to on the agent
  (§2 of `18-operations.md`), which refuses a mapping for a kind whose name settles its class. A
  device of a kind neither names is not offered, and the node's status lists it as unclassified
  rather than guessing;
- **its attributes**, the CDI spec's and device's annotations, which a request's selector matches;
- **the host device nodes its own edits give**, by which two devices are known to be the same
  hardware. Only a device's own edits count: the nodes its file gives with every device (NVIDIA's
  control node) say nothing of which hardware it is. Two devices are the same hardware when one's
  nodes contain the other's: NVIDIA's file names a GPU as `0` and each MIG partition of it as
  `0:0` and the like, each partition naming the GPU's node and nodes of its own, so the GPU and
  any partition of it are one hardware while two partitions are not. A device whose nodes are
  exactly those of two or more smaller devices of its file, as NVIDIA's `all`, is an aggregate
  and not offered, since a workload asks for a count instead.

### 6.2 Modes

| Mode | What a holder has | Offered by |
|---|---|---|
| `passthrough` | the device, alone, while the workload is placed | a driver declaring the class and the mode: the `oci` driver, for character devices |
| `timeslice` | the same device as up to `shares` other holders, scheduled by the device's own driver | the operator, per kind, as the number of shares (§2 of `18-operations.md`), and a driver declaring the mode; a kind not shared offers `passthrough` alone |
| `mediated` | a virtual device a hypervisor partitions the device into (vGPU) | a `vm` driver; none in this release |

A partition the hardware makes itself, as a MIG instance of an NVIDIA GPU, is a CDI device of its
own, given in `passthrough`. Timeslicing isolates nothing between its holders: their memory on the
device is not partitioned and one can starve another. That is what a host offering it sells, and
the explanation of a placement in that mode says so.

### 6.3 Requests, placement, and allocation

A workload asks in its intent (`devices`, §3.3 of `01-model.md`): each request a class, a count, a
label selector over the devices' attributes and ids, and a mode, `passthrough` when unset. The
filter keeps a node only where one driver offering the workload's tier declares the class and the
mode (§4.1 of `04-runtimes.md`) and enough matching devices are free. A device is free for
`passthrough` when no placement holds it or any device that is the same hardware (§6.1), and for
`timeslice` when fewer than its shares hold it in that mode and none holds it or the same hardware
in `passthrough`. The scheduler chooses the devices when it binds, in the same transaction: those a
placement following its target in place already holds first, then those sharing the least hardware
with other devices, so a partition is given before the whole device it is part of and the most stay
free, then the lowest ids; a workload that wants a whole device rather than a partition asks for it
by its selector. The placement records them (`Placement.devices`): what a node's devices hold is
derived from its placements, as `allocated` is. So a device is held while its placement is, freed
with it when the workload is retired or a move releases it, and restored with placements by a
restore (§8 of `03-state.md`), never a separate ledger that can disagree with them. The assignment
names the devices a placement holds, and the agent gives them to the driver in the spec by name,
the driver reading their edits from the files itself (§4.2 of `04-runtimes.md`). A device that
leaves the node's facts while held, because the vendor's toolkit rewrote its file or the card
failed, stays held by its placement; the agent refuses the next build that names it before
anything is made, as a driver refuses a target, and the driver refuses it at `Create` if the agent
did not. A workload not yet made on the node is then placed elsewhere (§2.1); one that ran there
stays, its start failing with the reason, until it is moved (§8).

> Corrected 2026-10-25 (Finding 169 of `23-walkthroughs.md`): this paragraph said the driver
> refuses the next `Prepare` naming such a device and the scheduler places the workload
> elsewhere. `Prepare` reads the intent and the recipe alone and never the devices held, and a
> refusal moves only a workload nothing of which was made on the node (§2.1), so the refusal is
> the agent's before `Create`, the driver's at it, and moving a workload that ran is migration.

Scoring keeps workloads that ask for no device off nodes that have devices when another
node will do (the `device fit` signal of §2.2), reading the node's devices rather than what its
driver could give.

### 6.4 Quota

Each class is a count dimension, `devices_gpu` and the like (§3.1 of `01-model.md`), enforced at
admission (§5.1): one for each device a workload holds, a `timeslice` share counting one as a
device in `passthrough` does, since what a quota bounds is how many a tenant holds, and what each
costs is the host's billing's (Rule K-12).

### 6.5 What it costs, and what it does not do

The CDI files are trusted as the node's own configuration: they are written by the vendor's
installer as root, and their hooks run as root on the host each time a container holding the device
is created, which NVIDIA's own file needs. So the agent and the driver, the files' two readers
(§4.2 of `04-runtimes.md`), read only files owned by root and writable by no one else, in
directories the same, and take a hook only when its program is the same: a device's hooks are host
code the host chose (§5 of `17-security.md`), and the driver reads them itself, so the agent, if it
were compromised, could name a device and never a hook. (Corrected 2026-10-25, Finding 169: this
sentence first named the agent the files' one reader, passing their edits to the driver.) Two of
CDI's edits are not applied, a device's Intel RDT class and the network interfaces it moves into a
container, and the driver refuses a device that needs either rather than giving it half. A workload
in a user namespace (every `container`, §2 of `04-runtimes.md`) is given a device node bound from
the host, which it opens as its host ids, so a node only its group can open (mode 0660, group
`video`) cannot be opened by it; NVIDIA's nodes are open to all, and a host whose devices are not
sets their mode with a udev rule. Which devices are the same hardware is inferred from the files
(§6.1): a vendor whose partitions name no node of their own has them seen as one device, held one
at a time, which errs toward giving less and never toward two holders of one; and a device whose
own edits give no node at all cannot be told apart from any other and is not offered. The devices
of a node are chosen request by request, in the order above, which is greedy: a workload whose
requests have overlapping selectors can be refused by a node whose devices would meet them paired
another way, and is told which request found too few. Devices are not metered yet (question 11 of
§10). A `microvm` is given no device, since Firecracker has no PCI passthrough (§4.1 of
`04-runtimes.md`), and a `vm` driver with VFIO, the path for one, is not built. And no real GPU has
been given to a workload on any machine this project is built on, whose GPU is never passed
through: what is tested is a stand-in device of a CDI file of the test's own (§4 of
`20-roadmap.md`).

---

## 7. When a node cannot satisfy an intent

> Resolves open question 5 of `02-architecture.md`.

The question was whether the agent may reject an assigned intent. Rejection is a decision, and §2
of `02-architecture.md` puts decisions in the control plane. But leaving an unsatisfiable intent to
manifest as silent non-convergence is worse, it looks identical to slowness.

**Resolution: the agent does not reject. It reports a fact.**

```
Observation
  state   = unsatisfiable
  reason  = missing_driver | insufficient_resources | volume_unavailable
          | device_unavailable | image_unsupported | prepare_rejected
  detail  = structured, from the driver's Prepare rejection
```

An observation is the agent's own domain; it is reporting what it sees, which is that it cannot get
there from here. The **scheduler** reads that observation and decides: reschedule elsewhere,
surface it to an operator, or leave it pending if the condition is transient.

> Corrected 2026-10-15, from the implementation. The decision to place elsewhere reads the
> journalled failure of the plan step the refusal stopped (§2.1), not the observation, which
> stays what the workload's status shows. An observation is the latest of a series and may be
> overwritten before it is read, and it does not say whether anything of the workload was made on
> the node, which decides whether moving it loses anything. A journal record is ingested exactly
> once, in order, and the agent marks it `step.refused` exactly when nothing was made.

The fact belongs to the agent. The decision belongs to the control plane. Nothing in §2 bends, and
the failure is visible within one observation interval instead of a timeout.

---

## 8. Migration

Rule K-4: resumable, verifiable, reversible, with explicit phases. Pterodactyl's transfer is a
single-shot imperative operation, which is why an interruption deadlocks the server into a state
requiring manual SQL (issue #4505), and why symlinks silently vanish (#5429).

```
prepare ─▶ replicate ─▶ quiesce ─▶ final_sync ─▶ verify ─▶ cutover ─▶ release
```

| Phase | Does | Interrupted here → |
|---|---|---|
| `prepare` | destination `Prepare` accepts; content pre-staged; volumes provisioned | source unaffected; destination cleaned up |
| `replicate` | bulk volume copy, repeated until the delta is small; workload keeps running | source unaffected; partial data garbage-collected |
| `quiesce` | stop the workload, or capture live state where `runtime_snapshot` exists | source restarts; migration abandoned |
| `final_sync` | ship the last delta | source restarts; destination discarded |
| `verify` | checksum the destination; confirm it can start | source restarts; destination discarded |
| `cutover` | the source releases its epoch, then the destination's `LeaseRequest` is granted a new one (§4.5 of `02-architecture.md`) | at most one side holds a valid lease at every instant; if the source cannot be reached the grant waits one term |
| `release` | after a hold period, delete source data | source data still present; rollback available |

Each phase is a desired state, not a command: the control plane sets the phase in both nodes'
assignments (`MigrationRole`, §4.8 of `02-architecture.md`), each node converges to its side of it
and reports completion, and only then does the control plane advance both. A lost update is
repaired by the next sync, and a node that restarts mid-phase resumes the phase it is assigned.

Four invariants:

1. **The source is authoritative until `verify` passes.** Nothing before cutover can lose the
   workload, because the destination is never authoritative until it is proven startable.
2. **Cutover is a lease epoch change** (§4.5 of `02-architecture.md`), so there is no instant at
   which two nodes are both authorized: a new epoch is granted only after the source has released
   the old one or its term has provably ended. This is the mechanism Pterodactyl lacks, and its
   absence is the direct cause of the deadlock class. What the lease guarantees is single
   ownership; how quickly traffic follows is a separate, network-level property, stated in §10 of
   `07-networking.md`.
3. **`release` holds.** Source data is retained for a configurable period after cutover. If the
   destination fails to start in production (which is exactly when it will), rollback is
   re-granting the source lease, not a restore from backup.
4. **Every phase is idempotent and resumable.** A migration interrupted at any point is resumed or
   rolled back by an operator or by the operation, never repaired by hand.

Symlinks, extended attributes, sparse files, ACLs, and hardlinks are part of the `verify` contract,
not incidental. Pterodactyl's #5429 (symlinks silently not transferred, so the workload will not
boot at the destination) is a verification failure, not a copy failure. Verification that only
checks file bytes will reproduce that bug exactly.

**Streams migrate with the workload.**

> Finding 3 of `23-walkthroughs.md`.

`14-streams.md` §1 argues that a console is evidence rather than a widget, and §4 puts its segment
files on the node. A migration that moves the volume and leaves the console behind on a machine
about to be decommissioned discards that evidence at exactly the moment it is most likely to be
wanted.

So stream segments travel in the same incremental transfer as the volume, in `replicate` and
`final_sync`, and are covered by the `verify` contract like any other data.

One extra thing crosses at `cutover`, alongside the lease epoch: **the stream's final offset**.
Offsets are monotonic per stream and are the seek primitive (§2 of `14-streams.md`), so a
destination that restarted numbering would break every reader's resume and silently change what a
stored offset refers to. The destination continues the sequence from the watermark it is handed.

Any range that could not be transferred is written as a **gap record**, the same marker used for
drops under pressure, so a migrated console states what it lost rather than appearing complete.

### 8.1 How a migration runs, as designed

Designed 2026-10-27 (P3.9a in §4 of `20-roadmap.md`), before it is built. The phases and the
invariants above stand; this says which component evaluates each phase, with what information,
what crosses between the two nodes, and what it costs. Trace 82 of `23-walkthroughs.md` follows it.

**A migration is asked for as a plan.** `WorkloadService.MigrateWorkload` names a workload and,
optionally, a node. With none, the scheduler chooses as it places (§2), among every node but the
one the workload is on; with one, the node is checked as `PlaceWorkload` checks it (§2.3) and the
caller needs `node.place` on it. Either way the caller needs `workload.migrate` on the workload, an
action implied by nothing (§5 of `10-api.md`): a grant that may restart a server was never one to
take capacity on a second machine and move the server there. The call proposes a plan of kind
`migrate` (§3.3 of `01-model.md`) whose one step, `migrate`, the control plane executes (§4.9 of
`02-architecture.md`). The step is disruptive at the vocabulary's floor, since `quiesce` stops the
workload, and a plan of kind `migrate` is never approved by a policy: a person approves it, or the
Operation that generated it (§3), whose own approval was a person's. The plan names the destination
with the scheduler's explanation, each volume with its size and what will cross for it (below),
whether `quiesce` stops the workload or captures its state, and every exposure whose address
changes (a `node_port` endpoint, §10 of `07-networking.md`). It is refused at proposal, saying why,
for a workload holding an address of a `node` pool (§3.7 there), a workload already migrating, one
with no placement, a destination failing any check of §2.1, and a local volume whose class cannot
cross between the two nodes: one sending no increments where either node's helper does not declare
`tree_sync` (§2 of `06-storage.md`), which it declares only where it finds rsync. **What evaluates
"this volume can cross":** the control plane, from the classes both nodes declared at their last
`Hello`.

**The migration is a record, and the destination a placement.** Approving the plan makes a
`migration` record when the plan becomes applying: the plan, the workload, the source and the
destination, the phase and when it was entered, the hold (a day unless the call says otherwise),
and, once it ends, its outcome, `completed`, `rolled_back`, or `abandoned`, with the reason. In the
same transaction, under the placement lock, the destination is bound as a second placement of the
workload, not authoritative and `bound_by: migration`, so its capacity, cores, and devices are held
from `prepare` on (§3.4 of `01-model.md`); a destination that no longer fits fails the step before
anything was done. A workload has one migration at a time.

**Each phase is advanced on both nodes' reports.** While a workload migrates both nodes hold an
assignment of it, each carrying a `MigrationRole` (§4.8 of `02-architecture.md`): the migration,
the role, the phase, the peer, and, per volume, how it crosses and the ticket of the pass under
way. The destination's assignment carries the target, the volumes, and no power, and its agent asks
for no lease, which the control plane would refuse it anyway while its placement is not
authoritative. Each node converges to its side of the phase and journals `MigrationProgress`
(migration, phase, side, done or failed, and the phase's detail); the control plane believes a
report only from the node assigned that side of that migration, about the phase the migration is
in, as it believes a replica's receive (§2.2 of `06-storage.md`), and records any other as
`journal.rejected`. It advances both assignments once the phase's condition holds:

| Phase | Source | Destination | Done when, as the control plane evaluates it |
|---|---|---|---|
| `prepare` | runs on | the target's `Prepare`, its content fetched, its ids, a place to receive each volume | the destination reports it |
| `replicate` | sends each volume, pass after pass | receives each pass | the destination holds a pass the source reported small, for every volume sent |
| `quiesce` | stops the workload, or captures its state; its stream writers seal | nothing | the source reports the object stopped and each stream's watermark |
| `final_sync` | sends what changed since the last pass | receives it | the destination reports the last pass held |
| `verify` | computes each volume's manifest | computes the same, and `Prepare` with the volumes in place | both manifests arrived and are equal, and the `Prepare` passed |
| `cutover` | releases its lease | is granted the next, starts | the placement moved, and the destination reports the workload running under the new epoch |
| `release` | deletes its copies, after the hold | nothing | the source reports them deleted |

A pass is small when it sent at most a hundredth of the volume or 64 MiB, whichever is larger, or
when it is the tenth. The control plane names each pass and evaluates it, from the bytes the source
reports it sent and the destination's report that it holds the pass, once both are in, in whichever
order they arrive: a pass held and small ends the volume's part of `replicate`, one held and large
starts the next, and `final_sync`'s one pass, started as the phase is entered, ends its part once
held. Neither node reports a copying phase done. (Corrected 2026-10-28, P3.9c1: designed with the
source evaluating smallness and reporting `replicate` done, which left the control plane to start
the next pass on the destination's report of a pass held before it knew whether the source would
send another, so a held report arriving first would have started a pass nobody needed; Finding 216
of `23-walkthroughs.md`.) A pass whose ticket runs out before the destination holds it is started
again under a new name. A node that restarts takes up the phase its assignment names, and each
phase can be done again: a pass sent again is received again, whole or from what is held, and a
manifest computed again is the same.

**What crosses, per volume.** Decided by the volume's class on the source:

- *A local class able to send increments* (btrfs, and ZFS once built): the replica transfer of §2.2
  of `06-storage.md`, the first pass whole, or, where the destination keeps a replica of the
  volume, an increment from the replica's snapshot, and every later pass an increment from the one
  before. The volume's own snapshots (§4 there) cross first, oldest first, each an increment from
  the one before it, so a migrated volume keeps them. At `final_sync` the pass is a snapshot taken
  stopped; at `cutover` the destination's last received snapshot becomes the volume, as a replica
  is promoted.
- *Any other local class:* the tree, by rsync: the source's helper runs the sender over the volume,
  and the destination's runs rsync's daemon side over the same connection, confined by its own
  `chroot` to the destination's copy of the volume, made at `prepare` under the destination's own
  identifier, keeping owners by number, hardlinks, ACLs, extended attributes, and holes, and
  deleting what the source no longer has. Each pass sends what differs, so `replicate` converges
  as increments do, though a pass taken from a running workload is inconsistent, which the
  `final_sync` pass, taken stopped, repairs. At `cutover` the copy is the volume already, and
  nothing is exchanged. (Corrected 2026-10-28, P3.9c2: designed with a staging directory exchanged
  for the volume at `cutover`, an exchange that guards a live volume, of which the destination has
  none; nothing runs on its copy until `cutover`, and receiving into it counts the bytes where they
  will stay.) Decided over GNU tar, which would send the whole tree
  every pass and so leave the workload stopped for a whole copy, and over a format of Korpis's
  own, which would be the one place the rules for hardlinks, ACLs, and holes were read differently
  from every tool that restores them; as for btrfs-progs, a node offering such a class needs rsync
  (§2 of `18-operations.md`).
- *A shared or block class:* nothing. The destination reaches the same storage (§2.1 of
  `06-storage.md`): a shared class's volume is the store's, and a block class's device is attached
  to the destination for the new epoch at `cutover` by the reconciler that attaches it at a
  failover, which fences the source.
- *A stream's volume* crosses as its class's local volumes do, so segments travel with the workload
  (Finding 3). The source's `quiesce` report names, for each stream, its next offset and the
  position in its changes log its agent has journaled to; the destination's assignment carries both
  from `cutover`, and its writer starts at the larger of the offset it finds and the one it was
  handed, writing a gap record between them when less arrived than was written (§4 of
  `14-streams.md`), while its agent follows the changes log from the position handed over, so no
  segment is journaled or offloaded twice.

Every transfer is sent through the mesh as a replica's is (§2.2 of `06-storage.md`): from the
source's mesh address to the destination's transfer port, which the destination's table admits
from the source's address alone while its assignment names the migration, under a ticket the
control plane's token key signs over a purpose label of the migration's own (`korpismt_`) and the
pass: the migration, the volume, the two nodes, the pass, the pass it is an increment from, and an
expiry, a transfer window after the pass started; both nodes' roles carry it. **What evaluates "this
stream may be written into this volume's staging, from there":** the destination's agent, from the
ticket and the token keys it holds, and its kernel, from the mesh's keys, as for a replica.

**`verify` compares manifests.** A volume's manifest is a canonical walk of it: every entry in the
byte order of its path, with its type, mode, owner and group by number, extended attributes with
the ACLs among them, and for a regular file its size, modification time, the SHA-256 of its
content, and its holes (the ranges `SEEK_DATA` and `SEEK_HOLE` find), for a symlink its target, for
a device node its numbers, and for a hardlink the first path of its group; its digest is the
SHA-256 of the walk. Both helpers compute it, the source over what it sent last, stopped, the
destination over what it received. **What evaluates "the destination holds what the source held":**
the control plane, from the two digests the two nodes journaled; each also reports its manifest in
runs of entries, each run's first path and digest, so where they differ the control plane records
the first path of the first run that differs, and the migration is abandoned. A volume in a shared
or block class is not compared, since there is one copy. This is the check Pterodactyl's #5429
lacked: a symlink, an attribute, or a hole lost in transit is a difference in the manifest, not in
the bytes.

**`cutover` is the lease.** The source's agent, its object stopped since `quiesce`, releases its
lease (§4.5 of `02-architecture.md`) and journals its meter's last sample of the workload at that
instant. On the release, or once the grantor's end of the source's lease has passed when the
source is not heard, the control plane moves the placement in one transaction: the destination's
becomes authoritative, the source's is not, and each local volume's record moves to the destination
under the identifier the destination's helper gave it. The destination's assignment then carries
the workload's power and the streams' watermarks; its agent asks for the lease and is granted the
next epoch (invariant 2), starts the workload, and reports it running. What follows the placement
follows it then, each by the reconciler that keeps it and only after the epoch changed: a block
device's attach, a cluster pool's address through its router (§3.7 of `07-networking.md`), a
stable endpoint's edges (§3.1 there), the ingress's upstreams, and the overlay's routes and
dependents (§10 there). A destination that cannot start the workload is rolled back.

**Abandon, rollback, and release.** Before `cutover`, any failed phase abandons the migration, as
`korpis migration abandon` does: both assignments lose the role, the source starts the workload
again if its power says so, the destination deletes what it received, and its placement is freed.
After `cutover` and until `release`, `korpis migration rollback` moves the workload back the same
way it came: the placement moves back in one transaction, the destination stops and releases its
epoch, and the source is granted the next epoch over the copies it kept, which hold what they held
at `quiesce` (the order corrected 2026-10-28, P3.9b2: designed with the destination's release
first, which would have let a silent destination stall the rollback, though the lease alone keeps
the two from both acting). What the workload wrote on the destination since `cutover` is not thrown
away: each moved volume's copy there is kept as a divergent copy, listed on the volume and deleted
only by a plan, as failover's is (§2.2 of `06-storage.md`). Once the hold passes, or `korpis
migration release` ends it early, the source deletes its copies of the local volumes, its placement
is freed, and the migration is completed; a shared or block class's volume is the destination's
already.

**The meter is cut at the epoch.** The source's samples are believed while it holds the lease, and
the last is the one it journals at its release; the destination's first is under the new epoch and
counts from zero, as the first sample on a node does; a sample of either node outside its lease is
`rejected` (§2 of `15-observability.md`). So no interval is counted by both nodes and none by
neither, and gaps are written for the authoritative placement alone. The copies the source keeps
through the hold are the installation's assurance of a rollback, not the tenant's storage: they are
in the source node's allocation, and in neither the meter nor the project's quota.

**Live state.** Where the source's driver declares `runtime_snapshot` and the destination matches
its `restore_requires` (§4.1 of `04-runtimes.md`), `quiesce` pauses the workload and captures its
state, `final_sync` sends the state with the last pass, and the destination restores it rather than
starting the workload; anywhere else `quiesce` stops it, and the plan says which. No driver
declares `runtime_snapshot` yet; the `firecracker` driver's is built with migration, in Phase 3 of
§4 of `20-roadmap.md`.

What it costs. Capacity on two nodes from `prepare` until `release`, a day by default. The
workload is stopped from `quiesce` until the destination starts it: the last pass, a manifest that
reads every byte on both sides at the disk's speed, and a lease's grant; a volume of 40 GB on disks
reading 1 GB/s adds about forty seconds of verification, and that is the price of #5429 not
recurring. rsync on every node with a local class that cannot send increments. Transfers share the
mesh with replication and are not shaped. What it does not do: move a workload holding a `node`
pool's address; move a retained volume its target does not attach, which stays where it is; or
move a workload between installations.

**Built 2026-10-28, the proposal and the record** (P3.9b1). `WorkloadService.MigrateWorkload`
proposes the plan, its one `migrate` step naming the source, the destination, the hold, each volume
with its size and how it crosses, whether `quiesce` stops the workload, and each endpoint whose
address changes; the plan waits for a person whatever the policy says. The destination is chosen by
the scheduler among the nodes serving the workload but its own, with the volumes of shared and
block classes as what it must reach, or checked as a placement by hand when named. Approving the
plan checks the destination again as it was chosen, which the step records (`chosen_by`): the
scheduler's choice against every filter of the scheduler, among the nodes serving the workload, and
only a node an operator named as a placement by hand is, its cordon and taints overridden (Finding
215 of `23-walkthroughs.md`). It binds the destination as a placement that is not authoritative
under the placement lock, and makes the migration record in `prepare` with each volume's crossing,
in the transaction that marks the plan applying; it is refused while another plan of the workload
applies, and every other plan of a migrating workload but a pruning is refused at approval. A
node's assignments and the metering gaps read the authoritative placement alone, so the destination
holds capacity and nothing yet runs there. `ListMigrations` reads a workload's migrations and
`AbandonMigration` ends one before `cutover`, the destination freed and the plan cancelled; rolling
back and releasing are refused before `cutover`. The CLI's `korpis workload move` and `korpis
migration get|abandon|rollback|release` are §2 of `13-surface-cli.md`. Tested over the API: the
plan, its approval holding both placements, a second migration refused, the abandonment; and the
refusals of a workload on no node, of its own node, and of a volume the destination's helper cannot
receive. What is not built yet: both nodes' roles in their assignments, the phases advanced on
their reports, `cutover`, rollback, and release (P3.9b2), and the nodes' transfers (P3.9c).

**Built 2026-10-28, the phases, the cutover, and the ends** (P3.9b2). Each node's assignment of a
migrating workload carries its `MigrationRole`: the side, the phase, the peer, each volume's
crossing with the source helper's identifier for it, and the hold's end. The node the workload is
not placed on authoritatively, the destination until the placement moves and the source after, is
a standby: its assignment carries the target and the volumes, the workload stopped, no plan, and
none of its node's endpoints, runs, or challenges. The source is told the workload stopped from
`quiesce` on, which is how it quiesces. A `MigrationProgress` record is believed as designed and
`journal.rejected` otherwise; each volume's pass, the bytes it sent, the destination's identifier,
and both manifests are kept, with the first path where the two manifests' runs differ, and the
source's watermarks from `quiesce`. A phase advances once each side acting in it reported it done
(the destination alone prepares and cuts over, the source alone quiesces and releases), `verify`
only where every volume sent arrived with an equal manifest; otherwise the migration is abandoned,
naming where. The cutover is tried in the transaction recording the source's release of its lease,
whether the release arrives on the stream or in the journal, and every heartbeat interval for a
lease that ran out: once nobody can act under the source's epoch, the placement moves, each
volume's record with it onto the destination's identifier, the quota counts the workload there,
the destination's assignment carries the watermarks, and the destination is granted the next epoch
by the ordinary lease rule. A volume sent whose copy the destination never reported rolls it back
instead. The destination's report of `cutover` starts the hold, and the source's report of
`release` completes the migration, its placement freed and the plan applied. A phase failed
abandons the migration before the placement moved, the plan failed, and rolls it back after. A
person's abandonment, rollback, or release takes the same transitions, the plan cancelled: a
rollback is refused once the hold has ended, since the source may have deleted its copies, and a
release until the destination reported the workload started. A rollback moves the placement back
with each volume onto the source's identifier and keeps the destination's copy of each volume
sent as a divergent copy (§2.2 of `06-storage.md`), diverged at the rollback and holding what the
source held at the move.

A silent node no longer stalls a migration (Finding 214 of `23-walkthroughs.md`). The sweep that
marks nodes unreachable ends one: a silent destination before the placement moved, or a silent
source before `cutover`, abandons it, the plan failed, so a workload whose source fell silent then
fails over by the ordinary rule (§2.4); a source silent at `cutover` is waited out, since its
lease's end is what the cutover needs; a destination silent at `cutover` after the move, never
having reported the workload started, rolls it back. During `release` a silent node is not acted
on: the source deletes its copies once it is heard again, and a person may roll back while the
hold runs. **What it costs:** the plan applies until the source deleted its copies, so a migrating
workload takes no other plan but a pruning through the hold, a day unless the call said otherwise,
and is not failed over from a silent destination until a person ends the migration; `korpis
migration release` ends the hold early. A source that never returns leaves the migration in
`release` until the node is removed, which ends it with drain and evacuate (P3.10). Tested over
the API with two nodes: every phase walked on both nodes' reports to completion after a release,
the watermarks handed to the destination and the volume moved; a rollback after the move, the
volume back on the source, the destination's copy divergent, and the source granted the next epoch
once the destination gave its own back; manifests differing at `verify`, the migration abandoned
naming the first path and the plan failed; and two reports out of place rejected. What is not
built yet: each node's side of every phase (P3.9c, P3.9d), and what follows the placement at
cutover besides the volumes and the lease (P3.9e).

**Built 2026-10-28, the passes and their tickets** (P3.9c1). The control plane starts each pass:
as `replicate` and `final_sync` are entered, each volume sent gets one, named by the control plane,
an increment from the last pass the destination held where the volume crosses by increments, and
whole or a tree's difference otherwise; both nodes' roles carry it with its ticket, signed as the
assignment is computed, expiring a transfer window after the pass started. A report about another
pass than the one under way records nothing of it. Each pass is judged as both reports of it are
in, as above, and the sweep that tends migrations starts again a pass whose ticket ran out. A
phase that sends nothing, every volume shared or block, is done as it is entered. The ticket is
of the same form as a replica's transfer ticket, its identifiers and expiry signed over a purpose
label of its own, and each is refused where the other is presented. Tested: the ticket, its
expiry, and its refusal as the other kind; the passes' judgement, the next pass's parent, and a
pass started again; and over the API, a large pass followed by a small one, the passes counted,
and every pass named by the control plane and carried with its ticket.

**Built 2026-10-28, a tree's passes** (P3.9c2). The helper declares `tree_sync` for each local
class where `/usr/bin/rsync` is present. `SendTree` starts rsync's sending side over a volume with
`-aHAXS --numeric-ids --delete --stats`, its remote shell the helper's own `splice` subcommand,
which joins the shell's standard streams to one end of a socket pair and ends when the far side
has, since rsync waits for its remote shell before it closes the shell's input; the other end is
the agent's to join to the transfer connection. `ReceiveTree` starts rsync's daemon side over the
other node's end, as a remote shell would, under a configuration of the helper's own naming one
module, the destination's copy, with `use chroot`, `numeric ids`, and no listing. `TreeStatus`
reads how either ended, the sender's with the bytes its statistics name. The helper starts no
thread, so clearing the close-on-exec flag of the one descriptor the sending rsync inherits and
closing it once the child holds it lets nothing else inherit it; the `splice` process adopts that
descriptor by number, the crate's seventh unsafe module. Tested as root between two classes: a
symlink, a hardlink, an extended attribute, an access ACL, a sparse file's holes, and an owner by
number kept; a second pass sending only what changed and deleting what the source deleted.

---

## 9. Rebalancing is never automatic

Korpis will not move a running workload to improve packing on its own.

Migration is disruptive by definition, even at its best, it stops a process, and for a game server
that means disconnecting players. A scheduler that optimizes utilization by moving customer
workloads without being asked is a scheduler that causes outages nobody requested and nobody can
predict.

Rebalancing is an **Operation** (§3): an operator asks for it, sees a plan showing exactly what
will move, where, and what it costs in disruption, and approves it, with `max_unavailable` and a
maintenance window. That is P5 and P9 applied to the one subsystem most tempted to violate them.

The scheduler may **recommend**. It surfaces imbalance, identifies consolidation opportunities, and
estimates what a rebalance would achieve. It does not act.

---

## 10. Open questions

1. **Preemption.** Should a `guaranteed` workload be able to evict a `best_effort` one to make
   room? It is what makes scavenger capacity useful and it means a running workload can be killed
   by somebody else's scheduling decision, which needs an explicit, visible contract with whoever
   bought best-effort capacity, not a silent behaviour. → here, once QoS classes are exercised
2. **Node pool autoscaling. Carried by §10 of `18-operations.md`, question 6.** Growing a cluster
   in response to pressure requires a provider interface (create a machine, install the agent,
   enrol it). That is close to "Korpis is not a cloud provider" and needs a boundary drawn before it
   is built.
3. **Weighting content locality against strategy.** Preferring a node that already holds a 4 GB
   image conflicts with `distributed`. There is no obviously correct exchange rate between "starts
   in 2 seconds" and "shares a failure domain", and the honest answer may be that it is an operator
   knob with a documented default rather than something Korpis decides. → here. **Resolved in
   §2.2, 2026-10-15:** an operator knob, the policy's weight for the signal, with a documented
   default of a quarter, low enough that it breaks near-ties and never overrides the strategy.
4. **Scheduling latency at scale.** Filtering and scoring five hundred nodes per placement is fine;
   doing it for a thousand queued placements during a mass migration is not. Agones batches
   allocation requests over ~500 ms specifically for this. Whether Korpis needs batching, or
   whether its placement rate is low enough that it never will, is a measurement not yet taken. →
   here
5. **Cross-organization spread. Resolved in §8 of `08-identity.md`**: explanations are filtered by
   the viewer's grants. A provider may want a customer's workloads spread across failure
   domains without revealing which nodes hold whose workloads to that customer. Spread constraints
   as written are evaluated by the scheduler with full visibility, but the resulting `Explanation`
   would leak placement information across tenancy boundaries. The explanation needs a tenancy
   filter.
6. **How a regional quota knows the region.** Resolved in §5.1 here, "Where a workload runs",
   2026-10-19: the second way, an entry naming the nodes it counts, checked at the bind and held by
   it. The question was: a selector matches the workload's labels (§5.1), because admission happens
   before placement and the labels are all it has. So "4 Gi in the EU" holds only if a workload
   that runs in the EU carries the label the entry selects, and nothing yet makes it: a workload
   labelled for nowhere is counted by the global entry, may be placed in the EU, and consumes EU
   capacity that the EU entry never counted. The answer has to bind the two, either by making
   placement honour the labels a counting entry selects on (a workload counted by `region: eu` is
   placed only where nodes say `region: eu`) or by admitting again at bind time against the entries
   that select the chosen node's labels. Neither is built; until one is, an entry with a selector
   on a placement property is advisory, and P4 forbids displaying it as a limit. → here, before
   quota sets are offered in Phase 2
7. **Held totals per quota entry.** Resolved in §5.1 here, 2026-10-19: totals kept beside each
   entry, moved by every recount of a workload, and reconciled at start and hourly. The question
   was: admission computes holdings from the intents on every declaration (§5.1), which does not
   scale to an entry on a large root organization. A total kept in the admitting transaction, and
   reconciled from the intents, is the remedy. It must exist before quota sets are offered in
   Phase 2, because that is when such entries become settable. → here
8. **Placement policy per organization.** Resolved in §2.2 here, 2026-10-22: an organization sets
   one for its subtree; a node is scored by the strategy and weights of the policy nearest its
   owner, a workload placed under the spread and affinity of the policy nearest its own
   organization, and candidates compared within the nearest owner first. The question was: the
   policy is one per installation (§2.2). A host that
   sells dedicated pools may want `packed` for its shared fleet and `distributed` for one
   customer's machines, and a customer with nodes of their own (§3.4 of `01-model.md`) may want
   to choose for those. A policy per organization is the obvious shape; what it does not answer
   is which policy scores a candidate when the workload's organization and the node's owner each
   set one. → here, before Phase 2
9. **The shortlist query and the placement lock.** Binding happens under one lock for the
   installation (§2.3), and a `FeasibilityQuery` is a round trip to an agent with a timeout. Asked
   inside the lock, every placement waits for the slowest shortlisted node; asked outside, the
   bind must re-check that what was queried still holds, and a node can change between the two.
   The second looks right, with the bind re-running the capability filter and the post-bind
   `Prepare` catching the rest, but it has not been built against a fleet. → here, before
   Phase 1's scheduling. **Resolved in §2.1, 2026-10-15:** outside the lock, before the binding
   transaction, which filters again with what the query found; the cost is up to five seconds
   added to an approval when a shortlisted node is silent.
10. **What churn counts, and over what window.** A crash loop on one workload and a node that
    lost every workload in a power cut are both churn, and they say different things about the
    node. Whether churn counts workloads lost or loss events, whether a crash counts as much as a
    failed node, and how long a loss is remembered decide whether the signal steers placements
    away from bad hardware or merely away from bad recipes. → here, before Phase 1's scheduling.
    **Resolved in §2.2, 2026-10-15:** loss events, each time the sweep marks a node unreachable
    while it holds workloads, one event however many; crashes do not count; remembered with a
    half-life of a day and forgotten after a week.
11. **Metering devices.** A device held is a resource a host prices, and the meter records none
    (§2 of `15-observability.md`): whether a held device is metered by the time it is held or by
    what the device's own driver reports it used, and whether a `timeslice` share is metered as a
    fraction, decide what a GPU host can bill from Korpis's records. Added 2026-10-25, when
    devices were designed (§6.5). → `15-observability.md`
