# Reconciliation

The protocol between the control plane and a node's agent: what a node is told, how it converges,
how leases keep two nodes from running one workload, and how what a node does reaches the record.
This contract outlives everything else, because once agents run on machines Korpis does not control
it is frozen for as long as those machines exist. Pterodactyl's panel and daemon are coupled by
version rather than contract, so the only way to write a compatible daemon was to imitate the
reference one (issue #141). Korpis's protocol is published and versioned so a third party can write
a conforming agent without reading Korpis's source.

## Contents

- [Level-triggered](#level-triggered)
- [The stream](#the-stream)
- [The assignment](#the-assignment)
- [Convergence is measured](#convergence-is-measured)
- [Adoption](#adoption)
- [Leases and fencing](#leases-and-fencing)
- [When the control plane is down](#when-the-control-plane-is-down)
- [Where steps run](#where-steps-run)
- [The journal](#the-journal)

## Level-triggered

The control plane never sends "start workload X". It sends the complete set of assignments for the
node, and the agent makes reality match it: creating what is missing, retiring what it is told to
retire, correcting what drifted. "Start X, stop Y" is correct only if no message is ever lost,
reordered or duplicated, and a dropped edge diverges forever, since nothing resends what the sender
believes happened. A whole-truth sync repairs a dropped message at the next sync.

So there is no restart command. A restart is a power declaration whose cycle is one higher; the
agent performs a cycle it has not yet performed and records the cycle beside the runtime object, so
a cycle is done once however often it is delivered. Deltas are allowed only as compression of the
same stream: a delta names the set digest it applies to, and a mismatch forces a full resync. The
agent never needs any particular earlier message.

## The stream

The agent dials out and holds one bidirectional stream for decisions and facts, never bulk data;
consoles, files and stream segments go through [relays](relays.md), so an upload never delays a
lease renewal. The agent opens with `Hello` (versions, driver capabilities, storage classes,
capacity, what it advertises for inbound traffic, the set digest it holds), and the control plane
answers `Welcome` (the protocol accepted, heartbeat interval, clock skew window, its clock, the
metering interval) and then the journal cursor.

| Control plane to agent | Agent to control plane |
|---|---|
| `AssignmentSet`, each assignment signed, with its digest; `AssignmentDelta` | `Ack` of the set digest received; `Resync` |
| `LeaseGrant`, `LeaseRevoke` | `LeaseRequest`, `LeaseReleased` |
| `RevocationList`, signed | `Observation` per workload |
| `JournalAck`, the ingested cursor | `JournalRecords`, in sequence order |
| `Relays`, `Peers`, `Edges`, `Replicas` | `NodeStatus`, the heartbeat |
| `FeasibilityQuery`, `ContentHint` | `FeasibilityResult`, `ContentStatus` |
| `Probe`, `ReachabilityProbe` | `ProbeListening` |
| answers to the node's requests | `CredentialRequest`, `JobAccessRequest`, `StepRequest`, `CertificateRequest` |
| `EdgeLeaseGrant`, `EdgeLeaseRefusal` | `EdgeLeaseRequest`, `EdgeLeaseReleased`, `EdgeChallengesHeld` |

Every request a node makes on behalf of a workload is answered only to the node that workload is
placed on (or, for an edge certificate, a member of the edge group serving it), so a node cannot ask
for another node's workloads. A node cannot know from the inside whether it is reachable; it reports
what it advertises, and the control plane measures with `ReachabilityProbe`.

## The assignment

The unit of desired state on a node is the assignment, one per workload placed there, signed:

```
Assignment
  workload, owner, name, labels   the last three unused by the node: what a restore recovers it under
  generation       monotonic per workload; any change increments it
  target           the intent the workload's plans reached, or the applying plan is reaching
  power            likewise: desired state, version, cycle
  continuity       on_lease_expiry, resolved
  shaping          quota-imposed state, such as an exhausted egress budget
  plan             the node's steps of the applying plan, through the last it may execute
  retire           present when the workload is to be removed from this node
  migration        the node's role, phase, peer, volume passes, watermarks and hold
  ids              the host id range its user namespace maps, unique on the node
  lease_epoch, recipe_manifest, volume_ids, endpoints, dependencies, reaches_api,
  challenges, runs, kept_runs, residual
```

- **The target is what a plan made, not the newest declaration, nor even the approved one.** A
  declaration awaiting approval reaches no node. Approval moves the target before anything runs, so
  a node told the approved target would run what no plan has made: a create approved for a later
  window would start without its pull, volume or install, and a crash restart would apply a change
  still waiting for its window. So the node is told the intent and power the plans reached, or that
  the applying plan is reaching. A workload no plan has brought anywhere is told its target with
  power stopped at version zero, so the steps have what they need and nothing runs.
- **`plan` carries ordered work without becoming a command.** The agent executes the next step up to
  `through`, reports its completion keyed by plan and index, and moves on, reading its assignment
  again before every step. Cancelling a plan removes it from the assignment, the lowest `through`
  there is. A completion is journaled before the agent records the step done, so a crash between
  runs the step again, a no-op the second time. A failed step is journaled and never passed; the
  control plane fails the plan. `reconfigure` validates the new target and changes nothing running:
  the runtime object is rebuilt from the target when next started, never later, never unannounced.
- **`retire` is how deletion differs from forgetting.** A retiring workload is stopped and its
  runtime objects destroyed; its volumes are untouched unless the deletion named them, which is its
  own irreversible step. Once the retirement is ingested, the assignment, placement and identity go.
- **A residual assignment** carries a plan for what a retired workload left on a node (deleting a
  retained volume, rolling one back, a snapshot), with no build, start, observation or lease. No
  lease fences those steps because what they touch is on that node alone, so a plan touching
  anything reachable from a second node is never carried this way.
- **`migration` makes each phase a desired state on each side.** The control plane advances both
  sides once the previous phase is observed complete; the destination holds a second assignment, a
  standby with no power, until cutover moves the placement. See [migration](migration.md).
- **The signature** lets an agent prove what it was told, which restore relies on, and lets no relay
  or proxy alter an assignment unnoticed. The agent believes a set only when every assignment
  verifies under a key enrollment gave it, names this node, names each workload once, and the
  digest matches; otherwise the set is refused whole, reported as a security event, and the old one
  kept. A delta is believed as a set is: each assignment it adds verifies and names this node, and
  the set it makes has the digest it claims. A believed set is kept on disk with its signatures and
  verified again at start.

**How a generation moves.** The control plane recomputes a node's set whenever something in it
changes and on every resync. An assignment whose content, digested with its generation zeroed, did
not change keeps its generation and signature; a changed one is sealed one higher. An assignment
leaving a node is kept inactive, so a workload placed there again continues its generation rather
than seeming to go backward. Computing one node's set is serialized, so two replicas never seal two
assignments under one generation. The set digest is over each workload and generation, sorted, as
`sha256:` hex.

An approved plan becomes applying when first assigned, in the transaction sealing the assignment
that carries it. A plan approved while another applies is queued, and preempts as [state](state.md)
says.

## Convergence is measured

Every observation reports what the agent converged to: the generation seen, the target intent's
identifier, the power version and cycle, and the last plan step completed.

- everything matches the assignment, no steps outstanding, and the state agrees with desired power:
  **converged**;
- the generation seen trails the generation at which what the node runs last changed: **applying**,
  with the exact lag;
- steps outstanding and the last reported within its timeout: **applying**, naming the step;
- generation matches, no step outstanding, and the state still disagrees: a real **failure**, shown
  as one, not as a spinner;
- no current observation: **unknown**, never a remembered value shown as current.

An observation is current only from the node the workload is placed on while that node is heard,
within three missed heartbeats. One that is not current is still shown, with its time and state
unknown. Generations that change nothing the node runs (dropping a finished plan, carrying an ACME
challenge) do not unsettle a converged workload: the control plane compares the node's report with
the generation at which the assignment last changed in anything but its plan and challenges. Three
states are computed in the control plane, never inferred from a node: **awaiting approval** (a
declaration ahead of the target with a plan proposed), **scheduled** (approved for a future window),
and **unplaced** (no node could take it, shown with its filtered explanation and retried).

## Adoption

A restarting agent must not kill anything. Every runtime object is labelled at creation with its
workload, intent identifier (never version, since a restore can issue a version twice), assignment
generation, power cycle and lease epoch. On start the agent enumerates every driver's objects,
matches them to assignments by workload, adopts matches without touching the process, and reconciles
differences (recreation being a disruptive step). An object with no assignment is reported as an
orphan and never deleted: "the control plane forgot it" and "it should be deleted" look the same
from the node, and one of them destroys customer data. A second object claiming a workload that has
one is an orphan too. An agent upgrade is a process restart, not a workload restart. Adoptions and
orphans are `node.adopt` and `node.orphan` effects; a node's own effects are named `node.<event>`
and a step's completion `step.<action>`, never the control plane's names.

Adoption comes first however long the driver takes: an agent whose driver cannot enumerate at start
acts on no assignment and asks again, from half a second doubling to fifteen. Reconciling without
adoption would build a second copy of every running workload beside itself. A node whose driver
stays silent runs what it ran, unsupervised, until its lease expiry tells the control plane. An
object the driver reports in an unknown state (a shim that does not answer) is neither started nor
stopped for up to a minute, then taken as stopped.

## Leases and fencing

Every running workload needs a valid lease: a workload, a node, an epoch (monotonic, from one
installation-wide sequence, never reused, signed), a term, and an expiry policy resolved from the
intent. Exactly one lease per workload is valid at any time. Pterodactyl has none, which is why an
interrupted transfer leaves two nodes believing they own a server and recovery is truncating a table
by hand (issue #4505). With leases, whoever holds the current epoch owns it.

Exclusivity rests on clocks, not delivery:

1. A lease is acquired or renewed by `LeaseRequest` with a nonce; the agent notes its monotonic
   clock when it sends, and the grant echoes the nonce.
2. The holder treats the lease as valid until `sent_at + term x (1 - rho)`; the control plane until
   `received_at + term`. Real time only moves forward between send and receipt, so the holder always
   believes its lease ends first, whatever the network delay.
3. Another node gets a new epoch only when the old lease is provably over: its holder sent
   `LeaseReleased` after stopping, or the grantor's end has passed. `LeaseRevoke` only hastens it.

The term is 60 seconds (configurable) and rho 1,000 parts per million, a margin against a clock
worse than it should be. Sixty seconds bounds a handoff from an unreachable node and how long a
`stop` workload runs after losing the control plane. The agent renews at a third of the term,
resends an unanswered request after five seconds with a fresh nonce, and ignores answers to old
nonces.

| The node asking | holds the lease | another node holds an unexpired lease | nobody does |
|---|---|---|---|
| is where it is placed | renewed, same epoch | `UNAVAILABLE`: retry once the old lease is over | a new epoch |
| is not | `NOT_ASSIGNED` | `SUPERSEDED`, naming the holder | `NOT_ASSIGNED` |

A renewal keeps the epoch, since the epoch labels runtime objects and travels in the assignment; an
issue increments it and is a `lease.issue` effect. `UNAVAILABLE` or `NOT_ASSIGNED` never shortens a
lease already held; `SUPERSEDED` ends it at once. An unspecified expiry policy is treated as `stop`,
the answer that cannot make two writers. A released lease is not asked for again while the
assignment says why it was released, a retirement or a migration's source past its cutover; once it
no longer does, as when the migration is rolled back, the source asks again. A workload that leaves
the set while it runs on the node, as a rolled-back migration's destination, is stopped and its
lease, still held and no longer renewed, released, so the node it went to is granted the next epoch
without waiting out the term; its object is then an orphan.

**Storage fences too, where it can.** Leases fence agents, not a disk a second node could attach. A
class reachable from several nodes declares `storage_enforced` fencing when its backend can refuse a
superseded writer (SCSI-3 persistent reservations, a Ceph exclusive lock with blocklisting), and
`lease_only` when it cannot; a shared filesystem is `lease_only`.

**What a fenced agent does depends on why.** Its epoch is stale and nobody holds a newer one (a
restored control plane, a healed partition): the request is answered with a reissue for the same
node, and the agent freezes mutations, keeps processes running, and resumes. Its epoch was
superseded by a grant elsewhere: the workload leaves its set and the refusal names the new holder,
so it stops the workload, keeps its data, and reports the volume as an orphan for an operator; two
running copies of a game server are two diverging worlds. The control plane always says which case
it is.

**The expiry policy is derived per workload.** `stop` when another node may start the workload on
one of its volumes (a class several nodes attach, or a replicated volume with automatic failover),
because two writers is data corruption; `keep_running` otherwise, because a game server's players
are still connected and nothing else can use its disk. A workload declaring automatic failover and
`keep_running` is refused, since failover moves only a workload that stops. The scheduler never
reschedules a `keep_running` workload automatically when its node becomes unreachable: it may still
be running, and a second copy is a split, not recovery. An operator may, through an evacuate
operation whose plan names the risk.

## When the control plane is down

| Capability | With the control plane down |
|---|---|
| running workloads | keep running |
| crash restarts, health checks, drift correction | keep working: the decision was already made (restarts back off from a second to five minutes, forgiven after a minute up) |
| leases with `keep_running` | honored until it returns |
| leases with `stop` | the workload stops when the lease expires, by design |
| plan steps already assigned | continue to the end of their assignment |
| effects, metering, break-glass use | journaled, delivered on reconnect |
| new intents, plans, placements, tokens | unavailable |
| established console and file sessions | continue for their token's lifetime |
| web, CLI, chat, API | unavailable |

The data plane keeps doing its job without the control plane; that is the difference between a panel
and a platform. New console sessions during an outage are a real gap; the break-glass socket on the
machine is the operator's answer, deliberately not anyone else's.

## Where steps run

| Executor | Runs | Carried by |
|---|---|---|
| `control_plane` | a control-plane worker | the job queue, keyed by plan and step |
| `node` | the agent in the assignment | the assignment's plan |
| `extension` | the provider extension that owns it | the extension contract, same key |

A plan's order is global across executors: step n reaches an assignment only once every step before
it has completed, wherever it ran, and a node's `through` stops before the first control-plane step
not yet done. A plan is applied only once every completion is ingested. Every step is keyed by plan
and index, so applying a step twice is a no-op that reports success and every retry is safe; drift
correction has no key and is idempotent because converging toward what is reached does nothing.
A control-plane step that fails is retried from half a minute, doubling to half an hour, and the
eighth failure fails the plan.

A node step may outlast a pass of the agent's loop (a backup reading a large volume), so each pass
asks the helper about the step's job, starts it only if none runs under its name, and records
completion once the job ended and its result is journaled. A job is named once per plan
(`bkp-<backup>`), so a restarted agent finds it rather than starting a second, and the agent forgets
a job only once no plan in its set names it.

## The journal

Most effects happened on a node, possibly while it was disconnected, so they cannot share a
transaction with the control plane. Every record a node originates is written first to a durable
local journal, numbered by a per-node sequence that survives restarts: effects, metering samples,
break-glass records, lease releases, and what the node did to volumes, snapshots, segments, backups
and replicas. Where order matters (a break-glass session) the record is written before the act.

The control plane ingests in sequence: it accepts record n only when the cursor is n - 1, advancing
the cursor in the same transaction as the rows the record produces. A duplicate is dropped; a record
beyond the next is not acknowledged, and the node resends from the cursor. A session begins with the
cursor, so a reconnecting agent knows where to resend from. A record is believed only as far as the
node's assignments reach: one for a workload or step the node does not hold is kept as
`journal.rejected` with the reason, and the cursor passes it, so one bad record never blocks a
journal. Believed effects are recorded with the reconciler as actor and the grant of the plan they
carry out, or for effects outside any plan (an adoption, a crash restart) the grant of the plan most
recently approved for the workload. A step's completion moves its plan; a driver's refusal before
anything was made there (`step.refused`) places the workload elsewhere.

Acknowledged records are kept for a retention window (seven days by default) so a restored control
plane can re-ingest them. A cursor lower than the node held means a restored control plane; a cursor
above everything the node numbered means the node's own disk is older, and the agent numbers on from
the cursor, a loss stated rather than hidden. A gap record stands for the numbers it names, for
three causes: the journal exhausted, records discarded before ingestion, or a damaged segment.

The journal is bounded (2 GiB by default) and the agent reports the horizon that buys, from its own
growth rate, in `NodeStatus`, so an operator sees how long a node can be cut off before audit and
metering have holes. Repeated identical effects are coalesced first (within ten seconds, with a
count; a crash can understate a run by one window, never lose a distinct event); then acknowledged
records inside their retention window are discarded, oldest first; only then are records lost, each
range kept durably and its gap record written from reserved space. Workloads are never stopped to
save the journal. On disk it is 16 MiB segments of length-framed records with a CRC-32C, synced
before an append returns; a torn tail is cut at start, and a bad checksum in an older segment ends
what it is believed to hold. The sender keeps a bounded number of records in flight, paced by
ingestion.
