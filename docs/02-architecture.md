# Architecture and the Reconciliation Protocol

**Status:** design **Date:** 2026-08-07 **Depends on:** [`00-overview.md`](./00-overview.md),
[`01-model.md`](./01-model.md)

---

## 1. Why this document matters more than the others

Almost everything in Korpis can be changed later. Storage engines can be swapped, UIs rewritten,
schedulers replaced, drivers added. One thing cannot: **the protocol between the control plane and
the data plane.** Once agents are deployed on machines Korpis does not control, that contract is
frozen for as long as those machines exist.

Pterodactyl's Panel and Wings are coupled by version rather than by contract. Issue #141, "support
for third-party daemons", was opened in October 2016 and answered the same day: it was already
possible "so long as the daemon you are using follows the same API style as the core daemon".
Closing it three days later, the maintainer added that supporting daemons from external authors is
difficult "unless said daemon is specifically designed to work with Pterodactyl". That answer is
the finding. The contract was whatever the reference daemon happened to do, so the only way to
conform was to imitate an implementation. It was not neglect; it is what happens when a protocol is
never designed as a protocol.

> Second external review, finding S56. Recorded in §17 of `23-walkthroughs.md`. An earlier version
> of this paragraph said the issue had been open, unanswered, since 2016. It was answered and
> closed within three days, and the answer supports the argument better than the claim did.

Rule K-7 requires that a third party be able to write a conforming agent without reading Korpis'
source. This document is what makes that possible.

---

## 2. The split

```
┌─────────────────────────────────────────────────────────────────┐
│  CLIENTS                                                        │
│  web · Discord · CLI · API consumers · extensions               │
│  Peers. None privileged. All speak the same API. (P2)           │
└───────────────────────────┬─────────────────────────────────────┘
                            │
┌───────────────────────────▼─────────────────────────────────────┐
│  CONTROL PLANE                                                  │
│                                                                 │
│  Owns: Organization, Project, Quota, Grant, Workload,           │
│        Intent, Plan, Recipe refs, Placement, Lease              │
│  Reads: Observation, Effect                                     │
│                                                                 │
│  Decides. Never commands.                                       │
└───────────────────────────┬─────────────────────────────────────┘
                            │  one long-lived stream per node,
                            │  dialled OUT by the agent
┌───────────────────────────▼─────────────────────────────────────┐
│  DATA PLANE, one agent per node                                │
│                                                                 │
│  Owns: Observation, runtime objects, local content store,       │
│        volumes, network namespaces, stream segments             │
│  Reads: Intent, Lease                                           │
│                                                                 │
│  Converges. Never waits to be told.                             │
└─────────────────────────────────────────────────────────────────┘
```

**The ownership rule is absolute.** Every object in `01-model.md` has exactly one writer. The
control plane never writes an `Observation`; the agent never writes an `Intent`. There is no object
both may modify. This is what makes "the panel says stopped but the server is running" structurally
impossible rather than merely unlikely (P1).

---

## 3. Connection direction: the agent dials out

**The agent opens its connections outward: one long-lived control stream to the control plane, and
one data connection to a relay (§3.1). Nothing ever connects to a node.**

Pterodactyl works the other way: the Panel makes HTTP calls to Wings, so every node needs a
reachable address, an open inbound port, and a certificate. That requirement is invisible in a
datacentre and crippling everywhere else.

Dialling out gives, all from one decision:

- **Nodes need no inbound ports.** No public IP, no port forwarding, no inbound firewall rule.
- **NAT and residential connections work.** A home machine, a machine behind CGNAT, a machine in
  someone else's cluster, all are ordinary nodes.
- **The firewall story is one sentence:** the node must be able to reach the control plane and its
  relays on 443. On one machine they are the same process and the same port.
- **Authentication is one-directional.** The agent proves who it is on connect. The control plane
  never needs credentials for the node.
- **Decisions and data travel separately, and both travel outward.** Assignments, observations,
  leases, and journal records share the control stream. Console bytes, file transfers, and stream
  segments share the data connection. Separating them is what keeps a bulk transfer from delaying a
  lease renewal, and dialling both outward is what keeps the node without a listening socket.

The cost is that the control plane and the relays hold many long-lived connections. That is a
well-understood engineering problem with well-understood solutions, and it is a far better problem
than requiring every operator to expose every node to the internet.

### 3.1 The data path: relays

> Second external review, finding S19. Recorded in §17 of `23-walkthroughs.md`.

Four documents sent clients **directly to the node**: web uploads (§8 of `11-surface-web.md`),
`korpis files sync` (§8 of `13-surface-cli.md`), stream readers (§4 of `14-streams.md`), and live
console keystrokes (§6 of `08-identity.md`). Under §3 a node has no inbound port and may sit behind
CGNAT, so a browser cannot reach it, cannot validate a certificate for it, and the design had no
component that could. The contradiction was real, and either half could have been given up. Giving
up §3 would make every residential and NAT-bound node second-class. Giving up the direct path by
routing bulk data through the control plane would make a 40 GB world upload a control-plane load
problem and make console availability depend on the database. Neither is acceptable, so the data
path gets its own tier.

**A relay is a stateless process that joins a client connection to a node's outbound data
connection.** It has no database, holds no grants, and evaluates no policy beyond verifying a
capability token.

```
client ──TLS──▶ relay ◀──TLS, dialled out── agent
          │                                    │
          │ capability token, verified offline │ the same token, verified again
          ▼                                    ▼
   routes the session to the node's data connection; never buffers it to disk
```

- **Every session is authorized twice, by the same token.** The gateway evaluates the grant chain
  online and issues a short-lived capability token naming one workload and one action (§6 of
  `08-identity.md`). The relay verifies its signature, expiry, and the revocation list before
  routing. The agent verifies it again before acting, so a compromised relay can observe sessions
  it carries but cannot invent one.
- **The relay is the first hop that sees the client's address**, so conditions that depend on it
  (`source_cidr`, §3.2 of `01-model.md`) are evaluated there, or at the gateway for API calls. An
  agent behind a relay never sees a client address and never evaluates one. Behind a load balancer
  the relay accepts the PROXY protocol from configured addresses only, because a header anyone can
  send is not an address.
- **Relays scale horizontally and fail independently.** A node keeps one data connection to each
  of a small configured set of relays, so losing a relay re-routes new sessions rather than losing
  the node. In-flight sessions on a failed relay end; streams resume from their last offset (§2 of
  `14-streams.md`) and file transfers resume from their last acknowledged chunk.
- **Console survives a control-plane outage for exactly one token lifetime.** The relay and the
  agent verify offline against the signing key and the last revocation list they received, so an
  established session continues until its token needs renewal, which requires the gateway. This is
  the bounded behaviour §6 of `08-identity.md` promised; before relays existed, nothing could
  deliver it.
- **On one machine the relay is a role of `korpis-server`,** enabled in the same process, speaking
  the same protocol. There is no separate thing to install until there is a reason to scale it.

**What a relay can see, stated plainly.** TLS terminates at the relay, so it sees the plaintext of
the sessions it carries: console output, keystrokes, and file contents in transit. A relay is
operator infrastructure with the same trust as the control plane, and it is deployed on machines
the operator controls. End-to-end encryption between a browser and a node would require every node
to hold a publicly trusted certificate, which is the inbound-reachability requirement §3 exists to
avoid. The cost is recorded in §8 of `17-security.md` rather than implied away.

**A direct path is an optimization, never a dependency.** A node that is reachable and holds a
certificate the client trusts may advertise a direct endpoint in `NodeStatus`. The CLI and SDKs,
which pin the Korpis CA, use it when present; browsers use it only when the node's certificate is
publicly trusted (§3.2 of `07-networking.md`). Every session works through a relay, so a node that
stops being reachable degrades in latency, not in function.

---

## 4. The protocol

### 4.1 Level-triggered, never edge-triggered

This is the single most important decision in the document.

The control plane does **not** send "start workload X". It sends **the complete set of assignments
for this node** (§4.8): for every workload placed there, the approved configuration, the desired
power state, the lease, and any plan steps, retirement, or migration role in progress. The agent's
job is to make reality match that set, creating what is missing, retiring what it is told to
retire, and correcting what has drifted.

Edge-triggered systems ("start X", "stop Y", "delete Z") are correct only if no message is ever
lost, reordered, or duplicated. Every real network violates all three. When an edge-triggered
message is dropped, the state divergence is permanent: nothing will ever resend "start X", because
from the sender's perspective it already happened.

Level-triggered systems self-heal. A dropped message is repaired by the next sync, because every
sync carries the whole truth rather than a delta from an assumed prior state.

The concrete consequence for a workload: **there is no `restart` command in the protocol.** Restart
is a new `Power` version whose `cycle` counter is one higher than the last (§3.3 of
`01-model.md`). The agent sees a power cycle it has not yet performed and performs it. If the
message is lost, the next sync carries the same `Power` and the same convergence happens. The
agent records the last cycle it performed beside the runtime object, so a cycle is performed once
however many times it is delivered. Nothing is missed and nothing is done twice.

> Second external review, finding S2. An earlier version of this paragraph described restart as
> `Intent.state: running` plus a `restart_epoch` field. Neither field existed after finding R7 moved
> power out of the intent body, and nothing carried `Power` to the node at all. §4.8 is the carrier.

Deltas are permitted as a bandwidth optimization, but only as a compression of a level-triggered
stream: every delta names the set digest it applies to, and a mismatch triggers a full resync. The
agent is never required to have received any particular earlier message.

### 4.2 The stream

One bidirectional stream, multiplexed. It carries decisions and facts, never bulk data: console
bytes, file transfers, and stream segments travel on the separate data connection of §3.1, so a
40 GB upload can never delay a lease renewal.

**Control plane → agent**

| Message | Purpose |
|---|---|
| `AssignmentSet` | the complete set of assignments for this node (§4.8), each signed, with the set digest |
| `AssignmentDelta` | optimization only; carries the base set digest it applies to |
| `LeaseGrant` | the answer to a `LeaseRequest`, echoing its nonce: an epoch and a term (§4.5) |
| `LeaseRevoke` | an optimization that hastens a release; safety never depends on its delivery (§4.5) |
| `RevocationList` | revoked token and grant identifiers, for offline verification (§6.3 of `08-identity.md`) |
| `JournalAck` | the highest journal sequence durably ingested, so the agent may discard up to it (§4.10) |
| `FeasibilityQuery` | a side-effect-free `Prepare` against a supplied manifest, for placement (§2.1 of `05-scheduling.md`) |
| `ContentHint` | recipe digests likely to be needed soon; prefetch |
| `Probe` | liveness check with a nonce |

**Agent → control plane**

| Message | Purpose |
|---|---|
| `Hello` | agent version, protocol version, driver capabilities, node capacity, the set digest it holds |
| `Observation` | current observed state of one workload, including what it has converged to (§4.3) |
| `NodeStatus` | capacity, allocation, pressure signals, driver health, reachability (§3.1) |
| `JournalRecords` | effects, metering samples, and break-glass records, in sequence order (§4.10) |
| `LeaseRequest` | acquire or renew a lease, carrying a nonce; the clock starts when it is sent (§4.5) |
| `LeaseReleased` | the node has stopped acting under an epoch; it is also a journal record |
| `FeasibilityResult` | the answer to a `FeasibilityQuery`: accepted, or rejected with a reason |
| `ContentStatus` | which recipe digests are present locally |
| `Ack` | the assignment set digest the agent has received |

> Second external review, findings S1 and S19. Recorded in §17 of `23-walkthroughs.md`. The first
> version of this table carried `AssignedIntents`, which could not express a power change, an
> approved-but-unapplied plan, a deletion, or a migration role, and it carried console traffic on
> the control stream while §4 of `14-streams.md` sent clients directly to nodes that, under §3, have
> no inbound port. Both are corrected by §3.1 and §4.8.

### 4.3 Convergence is measurable

Every assignment carries a monotonic `generation` (§4.8). Every `Observation` reports what the agent
has converged to, field by field: the `assignment_seen` generation, the `intent_seen` (the target
intent's identifier), the `power_seen` version and cycle, and `plan_progress`, the last plan step
it completed.

Those fields make the whole system honest:

- every reported field matches the current assignment, no plan steps are outstanding, and the
  observed state agrees with `Power.desired` → **converged**
- `assignment_seen` trails `generation` → the agent has not received or not yet applied the latest
  assignment. The UI says "applying", with the exact lag.
- `assignment_seen` matches, plan steps are outstanding, and the last one reported progress within
  its declared timeout → **applying**, and the UI names the step.
- `assignment_seen` matches and the observed state still disagrees with the assignment, with no
  step outstanding → the agent has the assignment and cannot satisfy it. This is a real failure
  and is surfaced as one, not as a spinner.
- No observation within the staleness window → `unknown`. Never a remembered value presented as
  current (P4).

Two states that are not about the node at all are computed in the control plane, never inferred
from an observation: a declared intent waiting for approval (`Workload.intent` ahead of
`Workload.target`, or the power head ahead of `Workload.power_target`, with a plan proposed for the
newer one, §3.3 of `01-model.md`) shows as **awaiting approval**, and an approved plan with a
future window shows as **scheduled**. Neither has reached a node, so no node can report it. The
third is **unplaced**: an approved target with no authoritative placement bound for it, because
no node could take it (§2.3 of `05-scheduling.md`). It is shown with the explanation of why every
candidate was rejected, filtered for the viewer (§8 of `08-identity.md`), and it is never a
spinner: the control plane retries it, and the state changes only when a node can take it.

> Second external review, findings S2 and S3. The first version compared `intent_seen` with
> `Workload.intent` and `Observation.state` with `Intent.state`. After finding R7 there was no
> `Intent.state`, and `Workload.intent` moved at declaration, before approval, so the comparison
> was either undefined or measured convergence toward something nobody had approved.

### 4.4 Adoption: a restarting agent must not kill anything

When the agent starts (after a crash, an upgrade, or a reboot), running workloads must survive.

Every runtime object the agent creates is labelled at creation with `workload_id`, `intent_id`,
`assignment_generation`, `power_cycle`, and `lease_epoch`. The intent is labelled by its
identifier, not its version number: identifiers are never reused, while a version number can be
issued twice across a control-plane restore, and a label that can mean two things cannot be
trusted to say which one was applied (§8 of `03-state.md`). On startup the agent:

1. Enumerates all runtime objects across all drivers.
2. Matches them to assignments by `workload_id`.
3. **Adopts** matches: takes over supervision without touching the process.
4. Reconciles differences: if the adopted object was created from an older target, apply the
   difference; if it can only be corrected by recreation, that is a step with a `disruptive` flag.
5. Reports objects with no matching assignment as orphans. It does **not** delete them
   automatically, because "the control plane forgot about it" and "this should be deleted" are
   indistinguishable from the node's point of view, and one of them destroys customer data.
   Deletion is the separate, explicit `retire` of §4.8, which a forgotten object never carries.

An agent upgrade is therefore a process restart, not a workload restart.

### 4.5 Leases and fencing

**Every running workload requires a valid lease. No lease, no execution.**

```
Lease
  workload      WorkloadID
  node          NodeID
  epoch         int          monotonic, per workload, never reused, signed by the control plane
  term          Duration     how long the lease lasts, measured by the holder (below)
  on_expiry     keep_running | stop       resolved from the intent, never defaulted here
```

The control plane issues exactly one valid lease per workload at any time. The epoch increments on
every issue.

This is what Pterodactyl lacks, and it is why its transfer failures deadlock. When a transfer is
interrupted, both nodes may believe they own the server, the panel cannot express that state, and
the recovery procedure is truncating a table by hand (issue #4505). With leases, the answer is
mechanical: whoever holds the current epoch owns it.

#### Exclusivity is a property of the clocks, not of message delivery

> Second external review, finding S7. Recorded in §17 of `23-walkthroughs.md`.

The first version of this section made exclusivity depend on `LeaseRevoke` arriving before
`LeaseGrant`, which is an edge-triggered message, the kind §4.1 says every real network loses, and
it expressed expiry as an absolute timestamp that the node compared against its own clock, which
§4 of `03-state.md` already treats as untrustworthy. Neither holds. Exclusivity now rests on a
rule that needs no synchronized clock and no reliable delivery:

1. **A lease is acquired or renewed by request.** The agent sends `LeaseRequest` with a nonce and
   notes the time **on its own monotonic clock at the moment it sends**. The grant echoes the nonce.
2. **The holder's lease runs from the send, the grantor's from the receipt.** The holder treats the
   lease as valid until `sent_at + term × (1 − ρ)`, where ρ is the declared bound on clock-rate
   error. The control plane treats it as valid until `received_at + term`. Real time only moves
   forward between sending and receiving, so the holder always believes its lease ends **before**
   the grantor does, whatever the network delay. This is the classical lease argument and it needs
   no shared clock.
3. **A new epoch for a different node is issued only when the old lease is provably over:** either
   the holder has sent `LeaseReleased` for that epoch, which it does only after it has stopped
   acting under it, or the grantor's own end of the old lease has passed. `LeaseRevoke` asks the
   holder to release early; when it is lost, the handoff waits one term instead of becoming unsafe.

So there is no window in which two holders are both authorized, and the claim no longer depends on
a message nobody can guarantee. The cost is a handoff that can take one lease term when the source
is unreachable, which is why terms are short and stated.

**The epoch check is in the data path wherever the storage allows it.** Leases fence the agents.
They do not, by themselves, fence a disk that a second node could attach. A storage class reachable
from more than one node declares `fencing: storage_enforced` when the backend can refuse a
superseded writer (SCSI-3 persistent reservations, a Ceph exclusive lock with blocklisting, an NFSv4
lease), and `fencing: lease_only` when it cannot (§2 of `06-storage.md`). A `lease_only` class is
honest about relying on the agents alone, and the default of the next section is chosen
accordingly.

#### What a fenced agent does depends on why it was fenced

> Second external review, finding S8.

The first version said in one paragraph that a fenced agent's workloads keep running, and in the
next that "everyone else stops", and §9 said a returning node stops its workloads. All three were
describing different situations without saying so. There are exactly two:

| Situation | How the agent learns it | What it does |
|---|---|---|
| **Its epoch is stale, and nobody else holds a newer one** (a restored control plane, a partition that healed before anything was reassigned) | its `LeaseRequest` is answered with a reissue for the same node | freeze mutations, keep processes running, accept the reissued epoch, resume. Fail-static (R12, §8 of `03-state.md`) |
| **Its epoch was superseded by a grant to another node** | the workload is absent from its assignment set and the refusal names the superseding node | stop the workload, retain its data, report the volume as an orphan for an operator decision. Another copy is authorized, and two running copies of a game server are two diverging worlds |

The control plane always knows which case it is in, because it issued the newer epoch or did not,
and it says so in the refusal. The agent never has to guess.

#### `on_expiry` is declared per workload, and its default is derived, not global

> Second external review, finding S8.

`on_expiry` decides what a workload does when its lease ends and cannot be renewed. The correct
answer differs by workload:

- A game server with local storage: `keep_running`. If the control plane is unreachable, players
  are still connected and stopping them serves nobody, and nothing else can use that disk.
- A stateful workload on storage another node could attach: `stop`. Two writers is data
  corruption. Losing availability is recoverable; losing the database is not.

So the field lives on the intent (`continuity.on_lease_expiry`, §3.3 of `01-model.md`), and when it
is not set the default is **derived** from the volumes the workload attaches: `stop` if any of
them is in a class that more than one node can attach, `keep_running` otherwise. The first version
said here that a global default would be wrong, while §8 of `03-state.md` and §5 of
`18-operations.md` named `keep_running` as the default; the derived rule is what all three now say.

**`keep_running` has a consequence for rescheduling, and it is enforced rather than hoped for.** A
workload that keeps running when its node is unreachable may still be running, so placing a second
copy elsewhere is not recovery; it is a split. The scheduler therefore never reschedules a
`keep_running` workload automatically when its node becomes unreachable. An operator may, through an
`evacuate` Operation (§3 of `05-scheduling.md`) that names the risk in its plan, and when the
original node returns it is in the second row of the table above.

### 4.6 What happens when the control plane is down

This is the payoff of P1, and it should be stated as a guarantee rather than a hope.

| Capability | Control plane down |
|---|---|
| Running workloads | **keep running** |
| Crash restarts, health checks, drift correction | **keep working**, the agent has the intent and does not need permission to execute a decision already made |
| Leases with `on_expiry: keep_running` | honoured until the control plane returns |
| Leases with `on_expiry: stop` | workload stops when the lease expires, by design |
| Plan steps already assigned to the node | continue to the end of their assignment; nothing new is assigned |
| Effects, metering, break-glass use | recorded in the node's journal and delivered on reconnect (§4.10) |
| New intents, plans, placements | unavailable |
| Established console and file sessions | continue through the relay for the remainder of their token's lifetime (§3.1) |
| New console and file sessions | unavailable: issuing a token requires the gateway |
| Web, CLI, chat, API | unavailable |

The data plane does not depend on the control plane to keep doing its job. That is the difference
between a panel and a platform.

New console sessions during a control plane outage are a real gap and are treated as one: the
break-glass socket of §10 is the answer for the operator, and it is deliberately not an answer for
anyone without a shell on the machine.

### 4.7 Idempotency

Every plan step is keyed by `(plan_id, step_index)`. Applying a step that has already been applied
is a no-op that reports success. Retry is therefore always safe, which is what allows every
operation to be retried rather than requiring a distinction between "failed" and "possibly failed"
(P9).

> Second external review, finding S4. Recorded in §17 of `23-walkthroughs.md`. The key was
> `(workload_id, intent_version, step_index)`, which is unique only for plans that create an
> intent. A power change, a drain, or a second migration of an unchanged workload produces the
> same key as an earlier plan, and "a repeated key is a no-op that reports success" then makes the
> second migration succeed without moving anything. A plan identifier is unique by construction.

Drift correction is not a plan and has no step key. It is idempotent for a different reason: it
converges toward the assignment, and converging toward something already reached does nothing.
The effects it emits are deduplicated by the journal sequence of §4.10, not by a step key.

### 4.8 The assignment: what a node is told

> Second external review, findings S1, S2, S3, and S5. Recorded in §17 of `23-walkthroughs.md`.

The first version of this protocol sent a node "the complete set of intents placed on it". That set
could say what configuration a workload should have, and nothing else. It could not say whether the
workload should be running (R7 had moved power out of the intent), which plan was being applied
(§7 of `03-state.md` needed to cancel one mid-flight), whether a workload had been deleted rather
than forgotten (§4.4 cannot tell the difference), or what a node's part in a migration was (the
seven phases of §8 of `05-scheduling.md` are ordered actions on two nodes). Each of those was being
described elsewhere as a message the protocol did not contain.

**The unit of desired state on a node is the assignment, one per workload placed there:**

```
Assignment
  workload      WorkloadID
  owner         {project, organization}
  generation    int            monotonic per workload; any change below increments it
  target        Intent         the approved configuration in full: Workload.target, never the
                               head; the agent needs the body to run it
  power         PowerRef       the approved power declaration: Workload.power_target, never
                               the newest; desired state, version, and cycle (§3.3 of 01-model.md)
  continuity    on_lease_expiry, resolved (§4.5)
  shaping       quota-imposed state, e.g. an exhausted egress budget (§3.1 of 01-model.md)
  plan          NodePlan?      present while an applying plan has steps this node executes
  retire        RetireRef?     present when the workload is to be removed from this node
  migration     MigrationRole? present during a migration: role, phase, peer
  signature     the control plane's signature over all of the above

NodePlan
  plan_id       PlanID
  steps         []NodeStep     the steps this node executes, in order, each with its index
  through       int            the last step index the node may execute

MigrationRole
  role          source | destination
  phase         prepare | replicate | quiesce | final_sync | verify | cutover | release
  peer          NodeID
```

The assignment is still level-triggered. It describes what should be true, and the node makes it
true; nothing in it is an instruction that is lost if a message is lost. Each part answers one of
the gaps above:

- **`target` is the approved intent, not the newest one.** A declaration that is waiting for
  approval has not reached any node, because it is not in any assignment. The agent therefore
  cannot converge toward a change nobody approved, which is the property P5 depends on and which
  the first version did not have (§3.2 of `03-state.md`).
- **`power` carries start, stop, and restart, once approved.** A restart is a higher `cycle`; the
  agent performs each cycle once and records that it did (§4.1). Like `target`, it is the approved
  declaration, so a stop waiting for confirmation has not reached the node (§3.3 of
  `01-model.md`, corrected 2026-10-13).
- **`plan` carries ordered work without becoming a command.** It states which steps of which plan
  this node is to have executed, up to `through`. The agent executes the next unexecuted step,
  reports its completion as an effect keyed `(plan_id, step_index)`, and moves on. **Cancelling a
  plan is lowering `through`** to the last step already started: the agent finishes the step in
  flight and does not begin the next. A lost update is repaired by the next sync, like everything
  else, and `plan.cancel` (§7 of `03-state.md`) is a change to desired state rather than a message
  that has to arrive.
- **`retire` is how deletion differs from forgetting.** A workload whose assignment carries
  `retire` is stopped and its runtime objects destroyed; its volumes are not touched, because
  deleting a volume is its own irreversible plan step (§3.6 of `01-model.md`). Once the agent
  reports the retirement complete and the effect is ingested, the assignment leaves the set, the
  placement is removed, and the workload's identity is retired (§3.3 of `01-model.md`). A
  runtime object with no assignment and no retirement is still an orphan (§4.4) and is still never
  deleted on the node's own initiative.
- **`migration` makes each phase a desired state on each side.** The control plane advances the
  phase on both assignments after it has observed the previous one complete, and every phase is
  idempotent (§8 of `05-scheduling.md`). The ordering lives in the control plane; the node only
  ever converges to "you are the destination, in `replicate`, from node 4".
- **`signature`** lets an agent prove what it was told. §8 of `03-state.md` uses that to recover
  assignments a restored control plane no longer remembers, and it means a compromised relay or
  proxy on the path cannot alter an assignment unnoticed.

The set digest of §9 of `10-api.md` is computed over the assignments, generation by generation, so
the resync optimization is unchanged.

### 4.9 Which steps run where

A plan's steps do not all run on the node. Allocating an address, holding quota, and asking a DNS
provider for a record happen in the control plane or through an extension. Each step declares its
executor:

| Executor | Runs | Carried by |
|---|---|---|
| `control_plane` | in a control-plane worker, claimed from the job queue (§6 of `03-state.md`) | the job queue, keyed `(plan_id, step_index)` |
| `node` | on the agent of the node in the assignment | `Assignment.plan` |
| `extension` | in the provider extension that owns it (§4 of `16-extensions.md`) | the extension contract, same key |

The plan's order is global across executors. The control plane does not place step *n* in an
assignment until steps before it have completed, whatever executed them, and it does not mark the
plan `applied` until every step's completion effect has been ingested. That is how a plan with steps
on two nodes and one extension is still one ordered, resumable sequence.

### 4.10 What the node records: the journal

> Second external review, finding S6. Recorded in §17 of `23-walkthroughs.md`.

Most effects are things that already happened on a node: a restart, a console keystroke, a
break-glass session, a drift correction during an outage. §1 of `03-state.md` promised that an
effect commits in the same transaction as the state change it describes, which is true of
decisions made in the control plane and cannot be true of events on a machine that was not even
connected at the time. Nothing made their delivery reliable, nothing acknowledged it, and the
effect table's primary key `(at, id)`, with `at` set at ingest, would accept a resent effect as a
new row.

**Every record a node originates is written first to a durable local journal**, numbered by a
per-node sequence:

```
JournalRecord
  node          NodeID
  seq           int            monotonic per node, never reused, survives agent restart
  kind          effect | metering | break_glass | lease_released
  occurred_at   the node's clock, informational
  body          the record
```

- **The agent writes the record durably before it acts on its consequence** where an order is
  required (a break-glass session is journalled before the shell opens), and immediately after the
  event otherwise.
- **The control plane ingests in sequence order.** It accepts record `seq` only if it is one past
  the highest it has ingested from that node, and it advances that cursor in the same transaction as
  the rows the record produces. A record at or below the cursor is a duplicate and is dropped; a
  record beyond the next is held until the gap fills. So node-originated effects are ingested
  **exactly once, in order**, without asking a partitioned table for a unique index it cannot have.
- **`JournalAck`** tells the agent the cursor. Nothing is discarded before it is acknowledged, and
  acknowledged records are kept for a further retention window (default seven days) before they
  are discarded, so that a control plane restored from an older backup can re-ingest them (§8 of
  `03-state.md`). The window is part of the journal's declared bound.
- **The journal is bounded, and the bound is declared.** A node partitioned for long enough fills
  it. Repeated identical events (a crash loop's restarts) are coalesced into one record with a count
  first; if the journal still fills, the agent writes a **gap record** naming the sequence range
  and the cause, exactly as streams and metering do (§3 of `14-streams.md`, §2 of
  `15-observability.md`). Workloads are never stopped to preserve the journal, because that would
  convert a full disk into an outage, and the loss is visible rather than silent.

The bound is the node-local buffering horizon that §7 of `15-observability.md` left open. The
default is 2 GiB per node, configurable. The horizon it buys depends on how busy the node is, so it
is not stated as a duration; the agent measures its own journal growth rate and reports the
resulting horizon in `NodeStatus`, so an operator can see how long each node can be cut off before
audit and metering have holes, rather than learn it afterwards.

---

## 5. Inside the control plane

Components, each with one responsibility and a defined interface. Nothing here reaches around
another component.

| Component | Responsibility |
|---|---|
| **Gateway** | Terminates all client connections. Authenticates the subject. Resolves and evaluates grants. Rejects anything unauthorized before it reaches the model. The single authorization path (P6). |
| **Planner** | Given a proposed `Intent`, computes a `Plan`: the steps, their reversibility, and the impact assessment. Pure, it reads state and produces a plan; it changes nothing. |
| **Scheduler** | Chooses a node for a workload, honouring constraints, quotas, driver capabilities, and placement policy. Records *why*. Detail in `05-scheduling.md`. |
| **Dispatcher** | Owns the agents' control streams. Publishes assignments and answers lease requests, ingests observations and journal records. Stateless, any replica can serve any agent. |
| **Store** | The authoritative state. Owns intents, plans, grants, and the effect log. Detail in `03-state.md`. |
| **Relay** | Joins client sessions to nodes' outbound data connections, verifying capability tokens offline (§3.1). Holds no state. Persists nothing: stream segments live on the node and offload from there (§4 of `14-streams.md`). |
| **Content** | Resolves recipe references to digests, verifies signatures, tells nodes what to prefetch. Detail in `09-recipes.md`. |

The Planner being pure is deliberate: it makes dry-run free, makes plans testable without a running
cluster, and prevents the class of bug where computing a plan has side effects.

### 5.1 What "resolves and evaluates grants" costs

> External review, finding R5. Recorded in §16 of `23-walkthroughs.md`.

One table row is not a design. Authorization here is a search for a permitting chain, and the
search is not small: a subject may hold grants directly and through N external identities, each
chain is d links deep, each link's validity is derived rather than stored (§3.5 of
`08-identity.md`), and a selector scope has to be evaluated against the current workload set on top
of that. Worst case is O(N × d) reads plus selector resolution, **per request**, and Bet 3 puts an
authorization in front of every plan computation, which is every click.

Caching is the obvious answer and it collides with the promise directly above it: revocation is
immediate. A cache with no invalidation design turns "immediate" into "eventually, by TTL", which
is the same class of claim this project refuses everywhere else.

So the closure is materialized and invalidated by event, not by clock:

| | |
|---|---|
| **What is materialized** | `subject → (action, scope, conditions)` closure, per subject, derived from the chains, each entry carrying the set of grant and binding identifiers its chains pass through |
| **What invalidates it** | a grant issued, revoked, expired, or suspended; a binding changing state (§5.1 of `08-identity.md`); a selector's membership changing |
| **How a change is recorded** | the transaction that makes it increments a single store-wide `authz_version` and appends one `authz_change` row naming the identifiers affected |
| **How a gateway learns of it** | from the snapshot every request already reads (below); `NOTIFY` on commit is a prefetch hint and nothing depends on it |
| **What happens on a miss** | full chain evaluation, which is the correct answer and is what a cold gateway always does |
| **What is never cached** | `max_uses` redemption (§6.2 of `08-identity.md`), `requires_mfa`, `requires_approval_by` |

> Second external review, finding S10. Recorded in §17 of `23-walkthroughs.md`. The first version
> carried invalidations on the job queue of §6 of `03-state.md`. That queue hands each job to
> exactly one worker (`FOR UPDATE SKIP LOCKED`), so it cannot tell every gateway replica anything.
> It also required invalidating every subject whose closure passed through a descendant of a revoked
> grant, which is the subtree walk that §3.5 of `08-identity.md` (finding R4) had removed from
> revocation, moved into the revoking transaction instead. And "a gateway serving a stale closure
> refuses" needed a store read on every request to know it was stale, which the table did not
> account for.

**Authorization is evaluated against the snapshot the request reads.** Every API request opens a
read transaction, because it reads the objects it acts on. The gateway reads `authz_version` in that
same snapshot. If its cached closure was computed at that version, the closure is exact for this
request, with no staleness window at all. If the version has moved, the gateway reads the
`authz_change` rows between the two versions, drops every cached entry whose identifier set
contains an affected grant or binding, and re-evaluates only those. Revoking a grant therefore costs
one revocation row and one change row, as R4 intended: the descendants of a revoked grant are
dropped because their chains contain its identifier, without anyone walking the subtree to find
them.

So revocation is **immediate** in the precise sense: no request whose snapshot includes the
revocation is authorized by a closure that predates it. A gateway that missed every `NOTIFY` is not
stale; it is merely colder. `authz_change` rows are retained long enough to cover the oldest closure
any gateway may hold, and a gateway whose cache is older than that retention discards it whole.

**The cost, stated.** One extra indexed single-row read inside a transaction the request opens
anyway, plus a range read when authority has changed since the last request. When the store is
unreachable no request can be authorized, so the API fails closed rather than authorizing from
memory; established relay sessions continue on their tokens (§3.1).

Selector scopes are the expensive case and they were already flagged as dangerous for a different
reason (§3.4 of `08-identity.md`, the set changes after the grant is issued). Both problems have
one answer: a selector grant's membership is materialized alongside the closure, so a workload
gaining a label is an invalidation event rather than a silent widening nobody evaluated.

The gateway is stateless and horizontally scaled with no leader (§5 of `18-operations.md`), so this
is a throughput cost rather than a bottleneck. It is stated because a design that puts one
authorization on the path of every operation owes a number, and "it evaluates grants" was not one.

---

## 6. Inside the agent

The agent is where Rule K-1 and P3 are either honoured or lost. The largest single class of
published Wings advisories (§1.1 of `research/evidence.md`) is a privileged process performing
filesystem operations on tenant-supplied paths, defending itself by inspecting path strings. The
structure below is designed so that class of bug has nowhere to live.

```
┌──────────────────────────────────────────────────────────────┐
│  Supervisor            unprivileged user, no CAP_*           │
│  Holds the control stream and the data connection. Runs the  │
│  reconciliation loop. Computes steps. Never touches tenant   │
│  files.                                                      │
└───────┬───────────────────────┬──────────────────────────────┘
        │                       │
        │ unix socket           │ unix socket
        ▼                       ▼
┌──────────────────┐   ┌────────────────────────────────────────┐
│  Privileged      │   │  Runtime drivers                       │
│  helper          │   │  oci · native · firecracker · qemu ·   │
│  root, tiny,     │   │  windows. Each declares its            │
│  socket-         │   │  capabilities and validates every spec │
│  activated       │   │  it receives (below). 04-runtimes.md.  │
│                  │   └────────────────────────────────────────┘
│  Verbs only:     │
│  mount, unmount, │   ┌────────────────────────────────────────┐
│  cgroup write,   │   │  Filesystem workers. ONE PER WORKLOAD  │
│  netns create,   │   │  In a mount namespace holding that     │
│  volume create,  │   │  workload's volumes and nothing else.  │
│  snapshot,       │   │  Unprivileged. Landlock-confined.      │
│  worker spawn    │   │  openat2(RESOLVE_BENEATH) for every    │
│                  │   │  path operation.                       │
│  No paths from   │   │  Serves the file manager and SFTP.     │
│  tenants. No     │   │  Cannot name a path outside the        │
│  arbitrary exec. │   │  workload's volumes, not by policy, by │
│                  │   │  kernel.                               │
└──────────────────┘   └────────────────────────────────────────┘
```

Three properties follow from this structure:

**No component both holds privilege and accepts tenant input.** The supervisor accepts
control-plane input and holds no privilege. The privileged helper holds privilege and accepts only
a fixed verb set with structured arguments, never a tenant-supplied path. The filesystem workers
accept tenant input and hold no privilege, and are confined to a subtree by the kernel, so a
traversal bug produces `EXDEV`, not a host file.

**File operations happen inside a namespace that contains only the files they may touch.**
Pterodactyl's Wings performs file operations for every tenant from one privileged process, which is
why a symlink race in one server's directory can reach the host. In Korpis, the helper's `worker
spawn` verb creates a fresh mount namespace, mounts into it the volumes of exactly one workload
(named by volume identifier, never by path), applies the Landlock ruleset, and starts the worker
inside it. There is no host path for the worker to reach, because the host filesystem is not in its
namespace.

> Second external review, finding S79. Recorded in §17 of `23-walkthroughs.md`. The first version
> ran one worker per **tenant**, inside "that tenant's mount namespace". A tenant has no single
> mount namespace; each of its workloads has one, and a stopped container has none at all, which is
> exactly when people edit configuration files. A worker spanning a tenant's workloads would also
> have to decide, by inspecting paths, that a share link scoped to one server may not reach its
> siblings, which is the defence by string that K-1 forbids. One worker per workload, in a namespace
> built for it, makes that decision the kernel's. Workers are socket-activated and exit when idle,
> so the cost is a process per workload in active use, not per workload in existence.

**SFTP and the file manager are the same worker.** They are the two highest-risk surfaces in any
panel, arbitrary paths, arbitrary names, arbitrary sizes, from untrusted users. Giving them one
confined implementation instead of two privileged ones removes the most productive bug source in
this product category.

**Where the volume is a block device the guest owns, the worker runs in a guest kernel.** A
`microvm` or `vm` volume is formatted and mounted by the guest (§3.1 of `06-storage.md`), so the
host cannot mount it while the guest runs, and mounting a filesystem image a hostile tenant
formatted would hand the host kernel's filesystem parser to that tenant. §4.4 of `04-runtimes.md`
gives the mechanism: the worker runs inside the workload's own guest while it is running, and inside
a disposable service microVM when it is not. The host never parses a tenant-formatted filesystem.

### 6.1 A driver is a privilege, and it guards its own door

> Second external review, finding S54. Recorded in §17 of `23-walkthroughs.md`.

The diagram makes the supervisor unprivileged, and it is, in the sense that it holds no capability
itself. What it can **cause** is bounded by what the drivers will do when asked, and the `oci`
driver holds the largest privilege on the node: access to containerd's socket is equivalent to root,
because containerd will create a privileged container with the host's root filesystem mounted if a
client asks for one. The first version of this section did not say so, and left the impression that
compromising the supervisor yielded an unprivileged process.

So the boundary is drawn at each driver, not at the supervisor:

- **Every driver validates every spec against the invariants of its tier**, independently of the
  supervisor, and refuses anything outside them: for `container`, user-namespace remapping,
  the seccomp profile, `no-new-privileges`, a read-only root filesystem, the dropped capability set,
  and no mount other than the volume mounts the helper prepared (§2 of `04-runtimes.md`). A spec
  the supervisor could not have produced from an honest assignment is refused, logged, and reported
  as a security event.
- **Each driver runs as its own user with access only to what its runtime needs**, so the union of
  driver privileges never lands in one process (§3 of `04-runtimes.md`).
- **Driver validation code is held to the same review standard as the helper**, because for the
  `oci` driver it is the only thing between a compromised supervisor and root. §6 of `21-stack.md`
  records what this meant for the language choice, and why it no longer splits it.

The honest statement is therefore: a compromised supervisor can ask for anything and obtain only
what an honest assignment could have produced. It does not reduce the oci driver's own attack
surface, which is containerd's, and §6 of `17-security.md` carries that negative.

### 6.2 The agent is several executables, and their protocol is versioned too

> Second external review, finding S55.

The supervisor, the helper, the filesystem workers, and each driver are separate executables
connected by local sockets. Those sockets carry a protocol, and the first version of this document
versioned the protocol between the agent and the control plane while saying nothing about the one
inside the agent, so an upgrade that replaced the supervisor but not the root helper had no defined
behaviour.

- **The internal protocols are protobuf schemas in the same published tree as the agent protocol**
  (§11 of `10-api.md`), governed by the same compatibility rules (§4 of `10-api.md`). A driver
  written by a third party conforms to the same contract as a first-party one (K-7).
- **The executables ship as one signed package and are upgraded together**, but no ordering is
  assumed inside a node: the supervisor and the helper each support the other's current and
  previous internal version, so a supervisor that restarts first talks to the old helper until the
  helper's next socket activation replaces it.
- **Filesystem workers hold no state**, so a running worker finishes its sessions on the old
  version and its successor starts on the new one. Drivers are upgraded one at a time and re-adopt
  their runtime objects through `Recover` (§4.2 of `04-runtimes.md`), which is the same path an
  agent restart already takes.

---

## 7. Transport, identity, and versioning

### Transport

Two connections over TLS 1.3, both dialled out by the agent: the control stream to the control
plane (§4.2) and the data connection to each of its relays (§3.1), the latter multiplexing one
flow-controlled stream per session so that one slow reader cannot stall another. Message framing is
a versioned binary schema, normative definition in `10-api.md`.

### Node identity

An agent authenticates with a per-node key established at enrolment. Enrolment uses a single-use,
short-lived token that is itself an attenuated grant (§3.2 of `01-model.md`), issuing a node token
is not a special code path, it is `node.enroll` scoped to one organization with `max_uses: 1`.
The node belongs to that organization from then on, and serves it and every organization below it
(§3.4 of `01-model.md`).

The node generates its Ed25519 identity key itself and sends only the public half at enrolment, so
the private key never leaves the machine. Every session then runs over TLS to the control-plane
certificate the agent pinned at enrolment, and its first message proves the other direction: the
`Hello` carries a signature by the node key over the node identifier, a timestamp, and a nonce,
which the control plane checks against the enrolled public key, a declared clock-skew window, and
the nonces it has seen inside that window (`NodeAuth` in `korpis/agent/v1/agent.proto`).

**No node ever receives a credential it does not need.** The critical advisory GHSA-pfvc-3p5h-x7h6
exists because Wings held registry credentials and a daemon token in a scope that tenant-authored
templates could reach. In Korpis, registry credentials are held by the Content component and
exchanged for **short-lived, digest-scoped pull tokens** issued per fetch. A compromised node
yields a token that can pull one already-known artifact for a few minutes.

The same rule governs backup and stream offload. A node never holds a repository's credentials or
its key-encryption key. For each backup job it receives the data key of the one workload being
backed up and a short-lived credential that can write new objects under that workload's prefix and
nothing else; where a target cannot issue such a credential, the upload is brokered through a relay
instead (§5.5 of `06-storage.md`). A compromised node therefore reaches the backups of the
workloads placed on it, which it could already read live, and no others.

### Protocol versioning (Rule K-7)

- The protocol is versioned independently of both the control plane and the agent.
- The agent declares its protocol version and driver capabilities in `Hello`. The control plane
  never infers capability from a version number, it asks, following Nomad's task driver interface.
- The control plane supports the current protocol version and the two preceding ones. Support
  windows are published, not discovered.
- Upgrade order is always: control plane first, then agents, one at a time. A mixed fleet is a
  supported state, not an emergency.
- The protocol definition is Apache-2.0 and published separately from the implementation, so that a
  third-party agent is a normal thing to build rather than a reverse-engineering exercise.

---

## 8. High availability

**The control plane is stateless.** Every replica can serve any client and any agent. Agents
connect through a load balancer and reconnect freely; there is no affinity between an agent and a
replica.

**State lives in the store**, which is the only stateful component and the only thing requiring a
replication strategy. Detail in `03-state.md`.

**Leases are held in the store with fencing epochs**, so replica failover cannot produce two
authorized writers for one workload. Exclusivity rests on the lease rule of §4.5, which needs no
coordination layer, and where the storage can refuse a superseded writer the epoch is enforced
there as well (`fencing: storage_enforced`, §2 of `06-storage.md`).

The honest framing: control plane HA matters **less** here than in comparable systems, because §4.6
guarantees the data plane survives a total control plane outage. Losing the control plane costs
management, not availability. That is a deliberate consequence of P1, and it means HA can be added
when it is needed rather than being a precondition for the first release.

---

## 9. Failure modes

Each row is a state the system must be able to represent and recover from without manual database
surgery, the standard Pterodactyl fails (issues #4505, #3332).

| Failure | Behaviour |
|---|---|
| Agent process dies | Workloads keep running. On restart the agent adopts them (§4.4) and resumes delivering its journal from the last acknowledged sequence (§4.10). |
| Agent host reboots | Workloads restart per their assignment. Adoption finds what systemd already restarted. |
| Network partition, node side | Agent keeps converging to its last assignments and journals what happens. Leases with `keep_running` persist; `stop` leases end at the holder's end of the term and those workloads stop. |
| Network partition, control plane side | Node marked `unreachable`. Observations go `unknown`, never a stale value shown as current. No new placements on that node. `keep_running` workloads are not rescheduled automatically (§4.5); `stop` workloads are, once the grantor's end of their lease has passed. |
| Node returns after its workloads were reassigned | Its refusals name the superseding node; it stops those workloads, keeps their data, and reports the volumes as orphans (§4.5). No split brain, and no data deleted on the node's initiative. |
| Control plane replica dies | Agents reconnect to another replica. No state is lost; the store is authoritative. |
| Relay dies | New sessions route through another relay; sessions on the dead one end and resume from their last offset (§3.1). |
| Store unavailable | The API fails closed, because no request can be authorized without a snapshot (§5.1). Agents keep converging and journalling; established relay sessions continue on their tokens. |
| A plan fails halfway | Completed steps are recorded as effects, and the resources they created are retained and named. The plan is `failed` and **resumable from its last completed step** (§3.1 of `05-scheduling.md`); it is not recomputed from scratch, because recomputing would discard the record of what was half done. |
| Migration interrupted | Source retains the lease until cutover completes and is verified. Interruption leaves the source authoritative and the destination's partial copy is garbage-collected. The workload never stops being owned by exactly one node (Rule K-4). |
| Two agents claim one workload | Impossible. One lease epoch is valid at a time, and a new epoch is issued to another node only after the old one is provably over (§4.5). |
| Agent is compromised | It holds a node key scoped to its own node, short-lived digest-scoped pull tokens, and per-job backup credentials for its own workloads. Its supervisor can request only specs its drivers will accept (§6.1). It cannot read another node's assignments, mint grants, or write effects attributed to another actor. |

> Second external review, finding S4. The "plan fails halfway" row said the next reconciliation
> computes a fresh plan and there is "no partially-applied limbo", while §3.1 of `05-scheduling.md`
> made a failed plan resumable from its last completed step and §7 of `03-state.md` computed the
> next plan against the partial state. Resumable is the stronger promise and the one P9 requires;
> this row now says the same thing as the other two.

---

## 10. Open questions

1. **Emergency console during a control plane outage. Resolved: a local socket, not a listener.**
   The framing assumed the operator reaches the agent over the network, which collides with §3, the
   agent has no inbound port, and opening one for an emergency would add permanent attack surface
   for an occasional need.

But if the control plane is down, the operator already has to reach the machine some other way, and
that way is SSH or a physical console. So break-glass is a **unix socket on the node**:
`korpis-agent console <workload>`, authorized by a break-glass token (§6 of `08-identity.md`) or by
being root on the box, with every use written to the node's journal before the session opens and
delivered to the effect log on reconnect (§4.10).

No network surface is added, nothing is cached that was not already deliberate, and the honest
limit is stated rather than hidden: **this does not work from a phone.** A mechanism that pretended
otherwise would be a listening socket on every node in the fleet, permanently, for an event that
happens twice a year.

2. **Does the agent cache grants at all? Resolved: no, it verifies tokens, and the distinction is
   the whole answer.** Settled in §6 of `08-identity.md`.

A grant is a policy that must be evaluated against a chain of parents, conditions, and scopes;
caching that on a node would mean putting the authorization engine and its inputs on the data
plane, where a compromised node could reason its way to a conclusion nobody authorized. A
capability token is the *result* of that evaluation (already performed, online, by the control
plane) signed, narrowly scoped, and short-lived.

So the agent holds no grants and evaluates nothing. It checks a signature and an expiry. Revocation
latency equals the token lifetime, which is the honest trade and the reason lifetimes are short.
3. **Full-resync cost at scale.** A node running several hundred workloads receiving the complete
   intent set on every reconnect is expensive. Content-addressed intent sets with digest comparison
   would let the agent say "I already have this exact set", but that adds a second identity concept
   to intents. **Resolved in §9 of `10-api.md`**, a set-level digest, not a per-intent identity.
4. **Multi-workload plans across nodes. Resolved in §3 of `05-scheduling.md`**: an `Operation` owns
   one plan per affected workload, generated as it proceeds. Draining a node produces migrations for
   many workloads. Is that one plan with many steps, many plans under a parent operation, or a
   distinct `Operation` object? This is question 1 of `01-model.md` and it needed settling before
   the scheduler was designed.
5. **Should the agent be able to reject an intent? Resolved in §7 of `05-scheduling.md`**: it
   reports a fact (`unsatisfiable`, with the reason) and the scheduler decides. A node may be unable
   to satisfy an intent it has been assigned, a driver is missing, hardware is absent, a volume will
   not mount. This surfaced as persistent non-convergence. An explicit `Reject` with a reason would
   let the scheduler reschedule immediately instead of waiting for a timeout, but it lets the data
   plane make a decision, which sits uneasily with §2.
6. **Observation frequency and cost. Resolved here.** Continuous streaming gives an accurate
   `since` timestamp and fast failure detection at the cost of constant traffic; on-change reporting
   with periodic heartbeats is far cheaper but blurs exactly when something changed. This was
   routed to `15-observability.md`, which never took it up (finding S18).

The blur was a false premise. `since` is the agent's own record of when the state changed, taken at
the transition on the node, so it is exact however late the report travels. What reporting
frequency actually controls is **detection latency**, and that has a separate answer: the agent
reports every state transition immediately, and the **node** sends a heartbeat (`NodeStatus`) at a
declared interval, default 30 s, carrying a digest of its workloads' current observations. While
the heartbeat arrives and the digest matches, every unchanged observation is current without being
rewritten; a workload's observation becomes `unknown` (§4.3) when its node misses three heartbeats.
Constant streaming buys nothing that on-change reporting does not already give, and a heartbeat
per workload would cost a store write per workload every interval for no information. Per node, it
costs one write per node per interval, which §9 of `03-state.md` counts.
