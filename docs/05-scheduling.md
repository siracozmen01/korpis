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
| satisfies the intent's constraints | `PlacementSpec` label selectors |
| the workload tolerates every taint | `Node.taints` |
| is `ready`: not cordoned, draining, or unreachable | `Node.status` |
| the tenancy's quota admits it | §5 |

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

**Scoring signals**, weighted:

| Signal | Prefers |
|---|---|
| utilization fit | per strategy |
| content locality | a node that already has the recipe digest, start latency instead of a download (Rule K-15) |
| volume locality | the node already holding this workload's volumes; usually decisive, since moving them is expensive |
| device fit | not consuming a GPU node for a workload that needs none |
| failure domain | per spread constraints |
| recent churn | penalize nodes that recently lost workloads, they may be unhealthy |

### 2.3 Bind: and record why

```
Placement
  workload, node, bound_at, bound_by, reason
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
  cpu       {reservation, limit}
  memory    {reservation, limit}
  disk      {reservation, limit}
  iops      {reservation, limit}
  bandwidth {reservation, limit}
  pids      {limit}                  ceiling only, nobody reserves the right to fork
```

`pids` has no reservation because process slots are not a capacity anyone sells; it exists solely
as a ceiling, and it is the one dimension with no graceful degradation on the way to it. A tenant
process that forks without bound exhausts the node's PID space and takes every other tenant on that
node down with it, the cheapest denial of service available to anyone with a shell. It is enforced
by cgroup v2 `pids.max`, it has a default, and there is no configuration in which it is absent.

- **Reservation** is guaranteed. It is subtracted from `Node.allocatable` and is never
  oversubscribed by anyone.
- **Limit** is the ceiling. The gap between reservation and limit is burst, and burst is what may
  be contended.

This yields three service classes that map exactly onto what hosting providers already sell:

| Class | Condition | Sold as |
|---|---|---|
| `guaranteed` | reservation == limit | "dedicated vCPU", "dedicated RAM" |
| `burstable` | reservation < limit | "shared vCPU", "fair-share" |
| `best_effort` | reservation == 0 | free tier, batch, scavenger capacity |

### 5.3 Overcommit is per-dimension, and never applies to reservations

| Dimension | Overcommit | Why |
|---|---|---|
| CPU | yes, configurable ratio | time-shared; contention degrades gradually |
| Memory | **discouraged, off by default** | there is no gradual degradation; there is the OOM killer |
| Disk capacity | yes (thin provisioning) | but see below |
| Disk IOPS | yes | time-shared |
| Bandwidth | yes | time-shared |
| Devices (GPU, passthrough) | no | exclusive by nature |
| Dedicated IP | no | exclusive by nature |

Overcommit applies **only to the burst region**. `allocatable` for reservations is always physical
capacity divided by one. A `guaranteed` workload is never oversubscribed, whatever the node's
ratio, otherwise the word means nothing.

Thin-provisioned disk carries a specific obligation under P4: a tenant told they have 100 GB must
actually be able to write 100 GB. So a node whose thin provisioning is approaching physical
exhaustion raises pressure, stops accepting placements, and alerts **before** any tenant hits a
limit they were never told about. Selling capacity that does not exist and discovering it when a
customer's write fails is the failure mode this exists to prevent.

Every one of these is kernel-enforced and metered, or it is not offered (Rule K-3).

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
2. **Node pool autoscaling.** Growing a cluster in response to pressure requires a provider
   interface (create a machine, install the agent, enrol it). That is close to "Korpis is not a
   cloud provider" and needs a boundary drawn before it is built. → `18-operations.md`
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
