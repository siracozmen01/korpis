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
| for the shortlist, the driver's `Prepare` accepts it | a `FeasibilityQuery` (§4.2 of `02-architecture.md`), side-effect free |
| has unallocated capacity for every requested dimension | `Node.allocatable` minus `allocated` |
| has every requested device available | §6 |
| satisfies the intent's constraints | `PlacementSpec` label selectors, required affinity rules |
| offers every storage class the intent's volumes name | `Node.storage_classes` |
| the workload tolerates every taint | `Node.taints` |
| is `ready`: active and connected, so not cordoned, draining, or unheard | `Node.admin`, `Node.connectivity` (§3.4 of `01-model.md`) |
| belongs to the workload's organization or one of its ancestors | `Node.organization` (§3.4 of `01-model.md`) |
| can be reached the way the workload's endpoints need | measured reachability (§9 of `07-networking.md`) |
| is in the overlay, when a dependency or a dependent is placed on another node | `Node.overlay` (§9 of `07-networking.md`) |
| the tenancy's quota admits it | §5 |

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

**And `Prepare` runs again after binding, on the bound node,** as the first thing the agent does
with a new assignment, because the node's state can change between the query and the bind. A
rejection there is the `unsatisfiable` observation of §7, and the scheduler re-places with that
node excluded and the reason in the `Explanation`.

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

**Scoring signals**, weighted:

| Signal | Prefers |
|---|---|
| utilization fit | per strategy |
| content locality | a node that already has the recipe digest, start latency instead of a download (Rule K-15) |
| volume locality | the node already holding this workload's volumes; usually decisive, since moving them is expensive |
| device fit | not consuming a GPU node for a workload that needs none |
| failure domain | per spread constraints, preferred affinity rules, and the intent's preferred region |
| recent churn | penalize nodes that recently lost workloads, they may be unhealthy |

Each signal scores a node from 0 to 1 and is multiplied by its weight (1 unless the policy says
otherwise); the highest sum wins, and a tie goes to the lower node identifier so that the same
inputs always choose the same node. **A signal is scored only where its information exists.**
Content locality needs the node's content inventory, which agents report as `ContentStatus`
(§4.2 of `02-architecture.md`), and recent churn needs the node's history of lost workloads,
which the control plane keeps itself: the crashes its agents observed and the placements it took
away because the node failed. Until that information exists, the explanation omits the signal
rather than scoring a guess.

> Corrected 2026-10-13, from the implementation. This said agents report both. No agent message
> carries a history of lost workloads, and none needs to: every loss is already an observation
> or a placement change the control plane recorded. Neither signal is scored yet. Both arrive
> with Phase 1's scheduling (§4 of `20-roadmap.md`); what window churn is counted over is
> question 10 of §10. Volume locality applies only to
a workload already placed, and failure domain only when a constraint, a preference, or a preferred
region applies to the workload.

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
about whether the workload can run there (a driver, a tier, capacity, a storage class, the
overlay) or its tenant's own declaration (constraints, affinity) or tenancy (the node's
organization), and a node failing one is refused with `UNSATISFIABLE` naming each check,
whoever chooses it. A draining or decommissioned node is not overridden: draining is the operator
emptying it. It needs `node.place` on the node and `workload.write` on the workload, and the effect
names the `node.place` grant and the reason. A workload already on a node moves only by migration
(§8), so placing it elsewhere is refused until migration exists in Phase 3. The cost is that an
operator cannot force a workload onto a node it does not fit, even when they know better than the
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
                 | node_upgrade
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
  placement      PlacementID
  status         pending | running | succeeded | failed | cancelled | missed
  exit_status    int?
  attempt        int
  streams        []StreamID       this run's own output
```

```
RunPolicy
  concurrency      allow | forbid | replace
  retries          int
  backoff          Duration
  timeout          Duration
  keep_successful  int      how many succeeded runs to retain
  keep_failed      int      failures are retained longer; they are the ones anyone reads
  catch_up         bool     run missed occurrences after downtime, or skip to now
```

`concurrency` and `catch_up` are the two that cause incidents when they are implicit. If last
night's backup is still running when tonight's is due: `forbid` skips it, `replace` kills and
restarts, `allow` runs both, and running two backups against one volume simultaneously is how
people lose data. If the control plane was down for six hours, `catch_up: true` fires six missed
runs at once, which is occasionally right and usually a thundering herd.

Each run holds its own streams (§3.6 of `01-model.md`), so "why did Tuesday's job fail" is
answerable after Wednesday's succeeded.

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
  reservations Resources          // guaranteed; sums against the parent
  limits       Resources          // ceiling; may exceed the parent, by declared ratio
  version      int                // participates in the admission CAS, see below
```

Admission requires an intent to fit within **every** matching entry. A tenant can hold a global cap
of 32 GB and a per-region cap of 4 GB simultaneously, and both are enforced. Composable, and it
replaces the flat `Quota` struct of `01-model.md` §3.1.

The dimensions are those listed in §3.1 of `01-model.md`, plus **dedicated IPv4 addresses**, a count
that is exclusive and never overcommitted. IPv6 addresses are not a dimension: they are abundant,
and counting them would make the scarce resource and the plentiful one look alike (§11 of
`07-networking.md`, question 4).

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
  evaluates this at declaration, from the tenancy tree it holds.
- **What a selector matches: the workload's labels.** They are the only description of the
  workload admission has; where it will run is decided later, by placement. Question 6 below is
  what that leaves open.
- **What a workload holds: per dimension, the larger of its head and its target.** The head may be
  approved at any moment and the target is what the kernel grants now, so holding either alone
  would let a workload awaiting approval, or one being shrunk, hold less than it may use. A limit
  left unset is the reservation (§5.2); an I/O ceiling left unset is unbounded, which no bounded
  entry admits. Disk includes every volume the workload creates.
- **Only growth is refused.** A declaration that holds no more than the workload already did, in a
  dimension, passes even where the entry is exceeded, so lowering a bound never strands a workload
  that then cannot be edited, not even to shrink it.
- **Holdings are computed from the intents, under the entries' locks**, never kept as a running
  counter that could drift from them; the entries' versions are bumped in the admitting
  transaction as the record that they were consumed. Entries are locked in identifier order and
  after the workload's own row, so two admissions never wait on each other in a cycle. A dry run
  performs admission and rolls back, so `korpis plan` shows a quota refusal before anything is
  sent for real.

What it costs: every declaration reads and decodes every workload under the entries that count it,
under a lock that serializes the declarations those entries count. For a project that is a handful
of rows. For an entry on a root organization at the comfortable scale of §9 of `03-state.md`, it is
up to 50,000 bodies per declaration, which is too slow to hold a lock over. The design that
survives that is a held total per entry, written in the same transaction as every intent and
recomputed from the intents by a reconciliation, so a drift between them is found rather than
trusted; it is not built yet, and question 7 below says when it must be.

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
   knob with a documented default rather than something Korpis decides. → here
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
6. **How a regional quota knows the region.** A selector matches the workload's labels (§5.1),
   because admission happens before placement and the labels are all it has. So "4 Gi in the EU"
   holds only if a workload that runs in the EU carries the label the entry selects, and nothing
   yet makes it: a workload labelled for nowhere is counted by the global entry, may be placed in
   the EU, and consumes EU capacity that the EU entry never counted. The answer has to bind the
   two, either by making placement honour the labels a counting entry selects on (a workload
   counted by `region: eu` is placed only where nodes say `region: eu`) or by admitting again at
   bind time against the entries that select the chosen node's labels. Neither is built; until one
   is, an entry with a selector on a placement property is advisory, and P4 forbids displaying it
   as a limit. → here, before quota sets are offered in Phase 2
7. **Held totals per quota entry.** Admission computes holdings from the intents on every
   declaration (§5.1), which does not scale to an entry on a large root organization. A total kept
   in the admitting transaction, and reconciled from the intents, is the remedy. It must exist
   before quota sets are offered in Phase 2, because that is when such entries become settable.
   → here
8. **Placement policy per organization.** The policy is one per installation (§2.2). A host that
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
   Phase 1's scheduling
10. **What churn counts, and over what window.** A crash loop on one workload and a node that
    lost every workload in a power cut are both churn, and they say different things about the
    node. Whether churn counts workloads lost or loss events, whether a crash counts as much as a
    failed node, and how long a loss is remembered decide whether the signal steers placements
    away from bad hardware or merely away from bad recipes. → here, before Phase 1's scheduling
