# Placement

How a workload gets a node: filter, score, bind and record why, and how it is placed again when its
node is gone. No other product in this market schedules: in Pterodactyl, Pelican, AMP, TCAdmin,
Multicraft and WISP a person picks a node from a dropdown, nodes drift into imbalance, new hardware
does nothing until someone moves things onto it, and "why is this server on that node" is answered
"someone chose it in 2023". Agones is the only nearby system with an explicit placement policy; its
vocabulary is taken, its operational cost is not.

## Contents

- [Filter](#filter)
- [Asking the driver](#asking-the-driver)
- [Score](#score)
- [Bind, and record why](#bind-and-record-why)
- [Failover](#failover)

## Filter

A node survives only if every check holds:

| Check | Against |
|---|---|
| a driver offers the requested tier, and its declared capabilities admit the intent | the node's drivers' capabilities, evaluated in the control plane |
| the driver runs what the intent's probes need (exec, the identity socket, the guest agent) | the driver's `exec`, `identity_dir`, `guest_agent` |
| capacity for every requested dimension | allocatable minus allocated, including the node ports its endpoints hold |
| every requested device is available | see [quota](quota.md) |
| the intent's constraints and required affinity | label selectors, affinity rules |
| every storage class the volumes name, and for a replicated volume a class able to send increments | the node's storage classes |
| every existing volume it attaches is held there | node-local volumes on that disk; shared classes by store; block classes by class |
| every taint is tolerated, and the node is ready (active, connected, not cordoned or draining) | taints, admin state, connectivity |
| the node belongs to the workload's organization or an ancestor | the node's organization |
| it can be reached the way the endpoints need, and serves the ingress where an `ingress` endpoint has no edge group | measured reachability |
| it reaches every node a dependency or dependent is on, through the mesh | the mesh's reachable pairs |
| a pool serving the workload reaches it with a free address, or the address it holds reaches it | address pools, for dedicated-address endpoints |
| it reaches every member of the edge group serving its stable or ingress endpoints | the mesh's pairs |
| every quota entry counting what runs on the node admits it | [quota](quota.md) |
| the driver's `Prepare` accepts it (for the shortlist) | a `FeasibilityQuery` |

`allocatable` is measured by the agent: capacity as the machine has it (a thousand millicores per
core, total memory) less a system reserve, by default 500 millicores and 1 GiB each capped at a
quarter of capacity, set per node at `korpis-agent run`. The reserve is what the system, agent and
runtime need; without it a node full on paper is overcommitted in fact. It limits nothing an
operator may run: it is a measurement the operator corrects. Disk comes from the storage classes:
capacity their size, allocatable what is free when the agent starts, so everything already on those
filesystems counts as taken and a restarted node reports less room than it has, never more.

## Asking the driver

Declared capabilities are compared first, where the scheduler is: every driver publishes as data its
tiers, architectures, image and disk formats, nested virtualization, Landlock ABI, kernel features,
volume semantics and device modes, and the content component resolves the recipe's manifest without
fetching a layer. That rejects most unsuitable nodes with a reason and no round trip.

Some questions only the driver can answer, so the shortlist is asked. Outside the placement lock and
before the binding transaction, the control plane scores the nodes as stored and sends the best
three a `FeasibilityQuery` carrying the intent and resolved manifest, waiting five seconds for all
together; `Prepare` is pure and fetches nothing. Each query carries a random identifier, and an
answer counts only from the node it was sent to. Only agents that say in `Hello` that they answer
are asked. The bind then filters and scores again under the lock with what the query found: a
refusing node is rejected by the `prepare` check with the driver's reason, a silent one is passed
over for that bind alone. A refusal is kept for the target, so later binds and operator choices
leave that node out; a new target is asked again. A node this process holds no session with is
ranked and bound unasked, as is one the query never reached, and is checked after the bind.

`Prepare` runs again as the first thing the agent does with a new assignment, since the node can
change between query and bind. When the driver refuses before anything of the workload was made
there, the agent journals `step.refused`, and the control plane keeps the refusal, undoes the
placement and places the workload again without that node, the plan still open. A workload an
operator placed by hand, or one with a volume or runtime object already on the node, has its refusal
journaled as `step.failed` and its plan fails there, since moving what it has is migration.

**What it costs.** An approval waits up to five seconds when a shortlisted node is silent, and a
slow node is passed over though it might have accepted. A driver that refuses a target today and
would accept it after an upgrade stays excluded until the target changes.

## Score

```
PlacementPolicy
  strategy   packed | distributed | balanced
  spread     []{topology_key, max_skew, scope: project | organization | label_selector}
  affinity   []AffinityRule
  weights    map[Signal]float
```

| Strategy | Prefers | Right when |
|---|---|---|
| `packed` | the fullest node that still fits | elastic capacity; consolidation enables scale-down |
| `distributed` | the emptiest node | fixed hardware; minimize blast radius |
| `balanced` | even utilization | mixed; the default |

Utilization is measured after the placement, per reserved dimension (CPU, memory, disk, egress
bandwidth), as the fraction of allocatable held. `packed` scores the mean, `distributed` one minus
the mean, `balanced` one minus the largest, so a memory-heavy workload avoids the node whose memory
is already the bottleneck.

**Spread** works on topology labels, independent of strategy: "no more than one of this customer's
three database replicas per rack". Pterodactyl's rigid two-level location could never express it. A
topology domain is a node label's value (`host` is the node itself unless labelled); a node without
the label is in no known domain, so it fails required affinity and anti-affinity and scores zero for
a preference, since treating it as a domain of its own would let an unlabelled node in the same rack
satisfy "apart by rack". Spread is a score, not a filter: a node exceeding `max_skew` scores zero
for the failure-domain signal, so a workload is still placed when only a skewed domain has room; a
constraint that must hold is a required anti-affinity rule. Spread and affinity count only the
workload's own organization (its project, its organization, or what the selector matches there), so
nothing reaches another tenant's workloads or lets a tenant steer by another's labels.

| Signal | Prefers |
|---|---|
| utilization fit | per strategy |
| content locality | a node already holding the recipe digest (weight a quarter by default) |
| volume locality | the node already holding the workload's volumes, usually decisive |
| device fit | not spending a GPU node on a workload that needs none |
| failure domain | spread, preferred affinity, the intent's preferred region |
| recent churn | not a node that recently lost workloads |

Each signal scores 0 to 1 times its weight (1 unless the policy says otherwise); the highest sum
wins, and a tie goes to the lower node identifier, so the same inputs always choose the same node. A
signal is scored only where its information exists, and the explanation omits it otherwise rather
than scoring a guess. **Content locality** is 1 for a node that reported holding the recipe (agents
report every recipe digest they hold in `ContentStatus`, each report replacing the last), 0 for one
that reported without it. It matches the recipe digest, not shared image layers, and anything that
ever removes content from a node must remove it from the report too. A node's report is believed as
its capacity is; a false one costs a slower start, not a breach. **Churn** counts loss events, not
crashes: one event when the silent-node sweep marks a node unreachable while it holds a placement,
however many it held; a loss weighs 1, halves daily and is forgotten after a week, and the signal is
1 / (1 + sum). Crashes are left out because they say more about a recipe than the machine. A
partition that silences a healthy node counts as a loss, since the control plane cannot tell.

**Whose policy.** The installation has one, `balanced` with every weight 1 by default, and an
organization may set its own for its subtree (`SetPlacementPolicy` under `org.write`, an effect).
The policy is read in two halves, each from the party whose decision it is. Strategy and weights say
how a pool of machines is used, the machines' owner's decision: a node is scored by the policy
nearest the organization it was enrolled into. Spread and affinity say how a tenant's workloads
stand toward each other, the tenant's decision: a workload is placed under the policy nearest its
own organization. Candidates are compared within one owner at a time, nearest the workload first, so
no two policies' scores are ever compared; a farther owner's node is chosen only when no nearer one
survives. So a customer's enrolled machines take its workloads while they have room, and a host's
dedicated pool takes its customer's workloads before the shared fleet. A policy is taken whole from
the nearest organization that set one, never merged, and moves nothing already placed. A policy
holds at most sixteen spread constraints and thirty-two affinity rules, and an intent thirty-two
affinity rules, because placement evaluates each against every candidate under the installation's
one lock, and an unbounded list would let one tenant hold every other's placements.

## Bind, and record why

```
Placement
  workload, node, bound_at, bound_by, reason
  intent        the approved intent it was bound for
  reserved      that intent's reservations, held on the node
  explanation   candidates, every rejection with its reason, every score, the chosen node,
                the runner-up, the policies as evaluated
```

Every placement is explainable afterwards: nineteen nodes considered, eleven lacked the microVM
tier, four failed memory, one was cordoned, three were scored, this one won on volume locality. A
scheduler nobody can interrogate gets overridden by hand until it might as well not exist, and the
explanation answers the support question that eats operators' time. Explanations are filtered per
viewer (see [capability tokens](capability-tokens.md)).

A placement is bound in the transaction that approves a target, so an approved workload has a node,
or a recorded reason it has none, before the approval commits; a scheduled window does not delay it,
since capacity is held from approval. Every bind and every cordon holds the installation's placement
lock from reading nodes to writing, so two binds never count the same free capacity and nothing
lands on a node cordoned mid-choice. Binds are therefore serialized across the installation.

**A placed workload follows its target in place, or not at all.** A new target for a placed workload
considers only its node, counting its own reservation as free, and a cordon does not stop it, since
a cordon stops new placements, not changes to what is there. If the node cannot hold the new target,
the placement stays bound for the old one and the workload is unplaced for the new one. Moving is
[migration](migration.md), never a side effect of an approval.

**No node can take it.** The newest failed attempt is stored with its explanation, the workload
shows as unplaced, and the control plane retries every such workload on a short interval, so a node
enrolled, uncordoned or freed is found without anyone approving again. A retry reaching the same
explanation changes nothing, so the record says since when this has been the reason.

**Manual placement** (`korpis workload place`, `bound_by: operator`) needs `node.place` on the node
and `workload.write` on the workload, and records the reason given. It overrides the node's cordon
and taints, which are the operator's own decisions about the node, and nothing else: every other
check is a fact about whether the workload can run there, the tenant's own declaration, or tenancy,
and failing one is `UNSATISFIABLE`, naming each check. A draining or decommissioned node is not
overridden. An operator cannot force a workload onto a node it does not fit; the remedy is to
correct the measurement.

## Failover

When the silent-node sweep marks a node unreachable (three missed heartbeats, ninety seconds by
default), the control plane takes each workload placed there whose expiry policy is `stop`, and once
the grantor's end of its lease has passed (last renewal received plus the term), places it again
under the placement lock with `bound_by: failover`, recorded as `workload.failover` naming the old
node, the new one and what it took. A `keep_running` workload is never moved this way, since it may
still be running. A workload no node can take stays where it was, saying why, and is tried at each
sweep. The sweep marks nodes under the placement lock, so a placement that chose a node while it was
heard binds before the node is found silent, never after, and no workload lands on a node already
marked and escapes the failover that follows.

It moves only where all its data is: it has no volume; each volume is in a shared class whose store
the new node offers; or in a block class whose provider fences the old node at the attach; or it is
replicated, declares `replica_failover: automatic`, and the new node holds its freshest replica,
whose age the effect records as `lost_since`. A workload whose only copy is on the lost node's disk
is not moved; an operator's evacuate may start it elsewhere from its latest backup or empty, its
plan naming the loss. It moves only while nothing of it is under way: a plan under way is the old
node's to finish or an operator's to cancel.

Nothing observes that the old node stopped it, since the node is silent. The control plane holds
when it received the last renewal, and the holder stops a `stop` workload by its own clock before
that end, so once it has passed a working agent has stopped. A block provider also refuses the old
node at the new attach, which holds even against an agent that is hung rather than cut off; a shared
class has only the clocks, which its `lease_only` fencing says.

The new node runs the workload from a `remake` plan the control plane writes and approves in the
moving transaction, under the grant of the decision it carries out: pull the recipe, attach each
volume where it is (promoting the replica, for a replicated one), never made anew or reinstalled,
and create and, if it should run, start the runtime. The workload starts from what its storage
holds, as after a power cut. When the old node returns, its epoch is superseded, so it stops what it
still ran and keeps any local data as an orphan. Failover never moves a workload back when its node
returns, which would be automatic rebalancing, nor moves one whose node is reachable but failing,
which is an operator's drain. For a replicated volume, the writes after the replica's last snapshot
are lost, as the workload declared it accepts, and kept on the old node as a divergent copy, not
merged.
