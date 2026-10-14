# State: Storage, the Event Log, and Consistency

**Status:** design **Date:** 2026-08-07 **Depends on:** [`01-model.md`](./01-model.md),
[`02-architecture.md`](./02-architecture.md) **Resolves:** open question 2 of `01-model.md`

---

## 1. Event sourcing is not needed here, and that is the important finding

The obvious design for a system built on Intent → Plan → Effect is full event sourcing: the event
log is the source of truth, and all queryable state is a projection rebuilt from it. It buys time
travel, perfect audit, and replay.

It was the wrong answer, and working out why changed this document.

Ask what history is actually needed for:

| Need | Provided by | Requires event sourcing? |
|---|---|---|
| Who did what, under what authority | the `Effect` log | no, an append-only table is enough |
| Roll back to an earlier configuration | the `Intent` chain | **no: intents are already immutable and versioned** |
| Explain an incident after the fact | `Observation` history | no: that is a time series, not an event log |
| Bill for usage | metering records | no: also a time series |

The second row is the finding. **`Intent` is already an immutable, versioned, parent-linked
chain.** The thing anyone would want to time-travel (the declared configuration of a workload) is
*already* stored as its complete history, by construction. Rollback is "point at intent version N",
not a replay of anything.

So event sourcing would pay full price, projections that drift, rebuilds measured in hours, event
schema migration, and debugging through an indirection, to buy something the model already has.

**Decision: PostgreSQL is authoritative. Intents are immutable rows, append-only by construction.
Effects are an append-only table. No projections, no rebuild, no replay.**

The transactional coupling is the part that matters, and it has two forms because effects have two
origins:

- **An effect of a control-plane decision** (a declaration, an approval, a grant issued, a denial)
  commits in the same transaction as the state change it describes, or neither commits. There is no
  window in which one exists without the other.
- **An effect of something that happened on a node** (a restart, a keystroke, a drift correction
  during an outage) cannot share a transaction with the event, because the event happened on
  another machine, possibly while it was disconnected. It is written first to the node's durable
  journal and ingested **exactly once, in journal order**, in the same transaction as the
  observation or state change it explains (§4.10 of `02-architecture.md`). Where the journal had to
  drop records, the loss is itself a record.

Audit cannot drift from reality in either case, which is what a separate audit-logging subsystem
(the usual design) cannot guarantee.

> Second external review, finding S6. Recorded in §17 of `23-walkthroughs.md`. The first version
> claimed the same-transaction coupling for every effect, which is impossible for the majority that
> originate on nodes, and nothing in the protocol made their delivery reliable or their ingestion
> idempotent.

---

## 2. Three data classes, three storage strategies

The mistake to avoid is treating all state as one kind of thing. Korpis has three, with different
volumes, lifetimes, and consistency requirements.

| Class | Objects | Volume | Consistency | Where |
|---|---|---|---|---|
| **Configuration** | Organization, Project, Quota, Grant, Workload, Intent, Plan, Recipe, Volume, Endpoint, Placement, Lease, Extension | small, highly relational | strong, transactional | PostgreSQL |
| **Audit** | Effect | high, append-only, never updated | strong at write, eventually archived | PostgreSQL, time-partitioned → object storage |
| **Metering** | metering records | moderate, one row per workload per interval | exact, idempotent, gap-marked | PostgreSQL, partitioned, the deliberate exception (§2 of `15-observability.md`) |
| **Telemetry** | Observation history, metrics | very high, low value per row | best-effort | **not the main database**, see §5 |

The last row is the one that kills systems that get it wrong. A thousand workloads reporting every
few seconds is millions of rows a day of data whose individual value approaches zero. Writing that
into the same database that holds grants and intents means the transactional store is dominated by
traffic that never needed a transaction.

Pterodactyl avoids this by not storing observations at all. CPU and memory graphs are polled live
over a websocket and discarded. That is why no Pterodactyl installation can answer "what was this
server doing at 3am on Tuesday". Korpis stores it, but not there.

---

## 3. Configuration state

Standard relational design in PostgreSQL. Three properties are non-obvious and load-bearing.

### 3.1 Intents are append-only by constraint, not by convention

```sql
CREATE TABLE intent (id             uuid PRIMARY KEY,
  workload_id    uuid NOT NULL REFERENCES workload(id),
  version        bigint NOT NULL,
  parent_id      uuid REFERENCES intent(id),
  declared_by    text NOT NULL,
  declared_at    timestamptz NOT NULL DEFAULT now(),
  body           jsonb NOT NULL,
  UNIQUE (workload_id, version));

REVOKE UPDATE, DELETE ON intent FROM korpis_app;
```

The privilege revocation is deliberate. Immutability enforced by the database cannot be
accidentally violated by application code, and application code is where every such invariant
eventually gets violated.

`body` is `jsonb` rather than fifty columns because the intent body evolves with every new runtime
driver, endpoint type, and health probe kind, and because it is validated against a schema before
insertion (Rule K-17) rather than by column types. Indexed expressions cover the fields that are
actually queried, placement constraints, recipe digest, lifecycle.

#### The body is protobuf, stored in its canonical JSON encoding

> Resolves open question 5 of §12 of `10-api.md` and 5 of §10 of `21-stack.md`.

The API carries the intent body as a typed protobuf message (§3 of `10-api.md`); the store holds
`jsonb`. Two representations of the same thing is how they drift, and an intent written two years
ago must still be readable, because rollback is re-declaring an earlier version (§4 of
`18-operations.md`).

There is one representation, not two: **`body` holds the protobuf message in proto3 canonical
JSON.** The schema is still the `.proto`; the column is that message in its defined textual
mapping. So it is queryable and indexable in PostgreSQL, it round-trips exactly, and there is no
second schema to keep in step.

Three details make it hold across versions:

- **Field names, not numbers.** Numbers would make the column unreadable and defeat the point.
  Names are just as stable, because §4 of `10-api.md` already forbids renaming a field in the wire
  schema, the rule that protects field numbers protects JSON keys for free.
- **Rows are never rewritten.** Intents are immutable, so a row is exactly the bytes that were
  validated at write. A field that a later version removed is still present in old rows and still
  reserved forever, so nothing reinterprets it.
- **`schema_version` is recorded on the intent.** Reading an old intent means parsing it with the
  schema that was in force when it was written, rather than hoping the current one still fits. This
  is what makes rollback to a two-year-old intent a defined operation instead of an optimistic one.

The compatibility rules of §4 of `10-api.md` therefore cover the database as well as the wire,
which is the property being bought: one schema, one set of rules, one place a mistake can be made.


**Defaults are written, not omitted.**

> External review, finding R10. Recorded in §16 of `23-walkthroughs.md`.

Proto3 canonical JSON omits fields that hold their default value. Combined with "rows are never
rewritten, so nothing reinterprets them", that produces exactly the outcome the rule was written to
prevent: a stored body that does not contain a field is not recording *the default at the time it
was written*, it is recording *whatever the default is when someone reads it*. Change a default in
2028 and every row from 2026 silently changes meaning while remaining byte-identical.

So intent bodies are serialized **with defaults emitted**. Rows are larger and their meaning is
frozen, which is the trade this whole section exists to make.

The rule that would otherwise be needed instead, "a field's default may never change", is added to
§4 of `10-api.md` as well, because it costs nothing and defends the same property from the other
side.

**Integers are strings, and the index has to know that.** Canonical JSON encodes `int64` and
`uint64` as strings. Expression indexes over those fields, and any range query on them, need an
explicit cast, and a cast that is not in the index definition is a sequential scan. The fields this
actually affects are few and known (byte counts, epochs, versions), so they are indexed with the
cast written into the index rather than discovered from a slow query log.
### 3.2 Conflicting declarations are detected, not merged

Two clients declaring intents for one workload at the same time is normal: a web user and a Discord
command, or an operator and an autoscaler. Last-write-wins would silently discard one of them.

Every intent creation is a compare-and-swap on the head of the chain:

```sql
UPDATE workload
   SET intent_id = $new, intent_version = $expected + 1
 WHERE id = $workload AND intent_version = $expected;
```

Zero rows affected means someone else moved first. The loser receives a conflict carrying the
intent that won, and must recompute its plan against the new base. This is the same rule as §3.3 of
`01-model.md`: a plan can never be applied against a state it was not computed for.

**Declaring does not change what any node runs. Approving does.** The head (`intent_id`) is what was
last declared; `target_id` is what was last approved, and only the target enters a node's
assignment (§4.8 of `02-architecture.md`). Approval is a second compare-and-swap, on the target:

```sql
UPDATE workload
   SET target_id = $plan_to_intent, target_version = target_version + 1
 WHERE id = $workload AND target_version = $expected_target
   AND intent_id = $plan_to_intent;          -- only the head is approvable
```

It commits in the same transaction as the plan's move to `approved` (or `queued`, §7) and the
effect recording who approved it.

Power has the same two pointers: `power_version` is the newest declaration and
`power_target_version` the approved one, and approving a power plan is the same compare-and-swap
on the latter, permitted only when the plan's power version is the head (§3.3 of `01-model.md`,
added 2026-10-13).

> Second external review, finding S3. The first version had only the head pointer, and the protocol
> published it, so an intent waiting for approval was already on its way to a node.

### 3.3 Authorization is not in the database

PostgreSQL row-level security is deliberately **not** used, despite fitting the tenancy model well.

Grants are evaluated in the Gateway and nowhere else (P6, §5 of `02-architecture.md`). Putting a
second copy of the authorization logic in database policies would create two places where authority
is decided, which eventually disagree, and the disagreement is a security bug that is invisible
until it is exploited.

One authorization path. The database enforces referential integrity and immutability; it does not
enforce policy.

---

## 4. The effect log

```sql
CREATE TABLE effect (id           uuid NOT NULL,
  at           timestamptz NOT NULL,
  plan_id      uuid,
  step         int,
  workload_id  uuid,
  node_id      uuid,
  actor        text NOT NULL,
  grant_id     uuid,
  action       text NOT NULL,
  outcome      text NOT NULL,
  error        jsonb,
  before       jsonb,
  after        jsonb,
  PRIMARY KEY (at, id)) PARTITION BY RANGE (at);

REVOKE UPDATE, DELETE ON effect FROM korpis_app;
```

- **Partitioned by time, on the ingest clock.** Monthly partitions. Detaching an old partition is
  instant; deleting millions of rows is not.

> External review, finding R8. Recorded in §16 of `23-walkthroughs.md`.

`at` is the time the control plane **ingested** the effect, from one clock, and it is what the
primary key and the partition boundary use. An agent's own reading of when the thing happened is a
separate column, `occurred_at`, and it is never a key of anything.

The first version of this schema did not draw that line, and a partition key fed by node clocks has
two failure modes that are not theoretical. A node whose clock is minutes fast writes effects into
a partition that does not exist yet or into the wrong month. And causal order inverts in the log
that exists to preserve it: `stop decided 12:00:05` above `container stopped 11:59:58`, in a table
whose entire purpose is being the record nobody can argue with.

Both readings are kept, because both are true and they answer different questions. `at` orders the
log and is monotonic within the control plane. `occurred_at` is what the node believed, it is what
a latency investigation needs, and a large gap between the two is itself a finding worth an alert.

```sql
  at            timestamptz NOT NULL,   -- ingest clock, PK and partition key
  occurred_at   timestamptz,            -- the agent's reading, never a key
```
- **Archived, never deleted.** Detached partitions are exported to object storage in a documented
  open format and remain queryable through an explicit archive path. Retention policy governs what
  is *online*, not what exists.
- **Every effect names the grant.** Not just the actor, the authority. `01-model.md` §3.3 explains
  why: "who did this" and "under what authority, delegated from whom" are different questions, and
  only the second one is useful during an incident. The control plane's own effects name the grant
  of the decision they carry out; a denial, which nothing authorized, names its subject and the
  action that was missing instead.
- **Effects are written by drift correction too.** Restarts, recreations, and repairs are recorded
  even though they produce no `Plan` (§3.3 of `01-model.md`). The plan log answers "what did
  someone decide"; the effect log answers "what actually happened". They are not the same question
  and the system must be able to answer both.
- **Node-originated effects carry their journal position.** `origin_node` and `origin_seq` record
  where an effect came from, and a per-node cursor makes ingestion exactly-once without a unique
  index on the partitioned table, which PostgreSQL could not enforce anyway because a unique
  constraint on a partitioned table must include the partition key (§4.10 of `02-architecture.md`):

```sql
  origin_node   uuid,                   -- null for control-plane effects
  origin_seq    bigint,                 -- the node journal sequence

CREATE TABLE node_journal_cursor (node_id  uuid PRIMARY KEY,
  ingested_through  bigint NOT NULL);     -- advanced in the ingesting transaction
```

---

## 5. Telemetry

Observations arrive constantly and are 99% uninteresting. They are split by how they are used.

**Latest observation: PostgreSQL, one row per workload, updated in place.** This is the
current-state cache the UI reads. Overwriting is correct: it is not history, it is "now". History
lives in the next row of this table.

The latest row is written on every state transition, not continuously and not on a timer; its
currency between transitions comes from the node's heartbeat (§10 of `02-architecture.md`,
question 6). That is real write load and it is counted in §9 rather than called zero.

**Observation history: a `metric` stream on the node.**

```
raw          full fidelity          hours
1-minute     downsampled            weeks
5-minute     downsampled            months
1-hour       downsampled            years
```

History is a stream of kind `metric` (§3.6 of `01-model.md`), owned by the agent like every other
stream. It is written on the node, downsampled there into the tiers above, migrated with the
workload (§8 of `05-scheduling.md`), offloaded with the workload's other streams (§4 of
`14-streams.md`), and read through the relay like a console's scrollback. "What was this server
doing at 3am on Tuesday" is a seek on that stream.

This keeps three promises at once that a separate time-series engine would have broken one of: the
control plane stays stateless (§8 of `02-architecture.md`), a single machine needs nothing more
installed (§1 of `18-operations.md`), and history follows the workload rather than the node it
happened to be on. Operators who want fleet-wide dashboards export metrics in Prometheus and OTLP
form to whatever they already run (§3 of `15-observability.md`); Korpis is not a monitoring stack.

State transitions (`running → crashed`, `healthy → unhealthy`) are **not** downsampled. They are
promoted into the effect log, because a crash is an event and belongs in the record that survives.
Downsampling loses the CPU curve, not the fact that something died.

**Metering is not telemetry, and it is in PostgreSQL.** It is the one measured series with financial
consequences, so it must not be downsampled, dropped under pressure, or reconstructed from averages
(Rule K-12). It is stored as one row per workload per metering interval, every resource a column,
which is low volume and high value, and `15-observability.md` §2 is its detail.

> Second external review, finding S11. Recorded in §17 of `23-walkthroughs.md`. This section put
> metering in "a separate append-only series" outside the transactional store, while §1 of
> `15-observability.md` put it in PostgreSQL as the deliberate exception. The second is right: a
> number that becomes an invoice needs transactions, idempotent re-delivery, and period closing,
> and those are the store's properties. The time-series engine this section deferred is not
> needed at all, for the reasons above.

What is decided here is that observation history is **not in the transactional store**, and that
observations never enter a transaction with configuration state.

---

## 6. The job queue, and killing the SQLite assumption

An earlier design proposal built the job queue on PostgreSQL's `LISTEN/NOTIFY` and `SELECT … FOR
UPDATE SKIP LOCKED`, and in the same breath said a SQLite path would stay open for small
installations. Those two statements contradict each other: both mechanisms are PostgreSQL-specific.
`01-model.md` recorded this as an open question. It is resolved here.

**Decision: PostgreSQL is the only supported store. SQLite is not supported. MySQL and MariaDB are
not supported.**

The alternative (abstracting the queue so several engines work) was rejected for reasons that are
worth stating, because "support more databases" always sounds generous:

- `SKIP LOCKED` and `LISTEN/NOTIFY` together give a correct, fast, dependency-free job queue. An
  abstraction that also runs on SQLite must degrade to polling, and the degraded path becomes the
  one that is under-tested and where the subtle bugs live.
- Multiple engines multiply the test matrix forever, and the multiplication compounds with every
  feature. Pelican added SQLite, MariaDB, and PostgreSQL drivers, which is three code paths for
  every query, permanently.
- `jsonb`, partial and expression indexes, partitioning, `generated always as identity`, and
  transactional DDL are all used above. A portable subset would mean giving up most of them.

The cost is real and is accepted: an operator must run PostgreSQL. `00-overview.md` requires that a
single operator with one machine can install and run Korpis, and that requirement is met by
shipping PostgreSQL as part of the installation rather than by weakening the store. One database
that always behaves the same way is worth more than three that mostly do.

**The queue itself:**

```sql
CREATE TABLE job (id            uuid PRIMARY KEY,
  key           text UNIQUE NOT NULL,   -- idempotency key
  kind          text NOT NULL,
  payload       jsonb NOT NULL,
  run_after     timestamptz NOT NULL DEFAULT now(),
  attempts      int NOT NULL DEFAULT 0,
  max_attempts  int NOT NULL DEFAULT 10,
  locked_by     text,
  locked_until  timestamptz,
  status        text NOT NULL);
```

Workers claim with `FOR UPDATE SKIP LOCKED`, wake on `NOTIFY`, and fall back to polling on a slow
timer so a missed notification delays a job rather than stranding it, level-triggered, for the same
reason as §4.1 of `02-architecture.md`.

`key` is `(plan_id, step_index)` for plan steps, which makes enqueueing the same work twice a no-op
and satisfies the idempotency requirement of §4.7 of `02-architecture.md` directly rather than by
discipline. The key was `(workload_id, intent_version, step_index)` and collided for every plan that
does not create an intent (finding S4).

No Redis. No NATS. No broker. They are added if and when a measurement demands it, not because the
architecture diagram looks more serious with them.

**As built, 2026-10-14**, when prunes made it the first work the control plane does itself. The
table above was made in Phase 0 and is now used as drawn, its statuses `pending`, `running`,
`succeeded`, `failed`, and `cancelled`. A claim is one statement: the oldest due job of the kinds
a worker takes, locked with `SKIP LOCKED`, marked running under the worker's name until its lease
runs out, its attempt counted; a job whose lease ran out is due again, so the work of a replica
that died is taken by another, and the first holder can no longer end it. A job is queued in the
transaction that decides it is due and ended in the transaction that records what it did, so
neither half can happen without the other; a retry puts it back for a later time, and one past
its last attempt fails instead. Queueing announces on `korpis_job`, and workers also look every
five seconds, because a retry's time comes by the clock rather than by an announcement.

---

## 7. One plan at a time, and how the other one is stopped

> External review, finding R6. Recorded in §16 of `23-walkthroughs.md`.

Making the diff a first-class object (Bet 3) buys dry-run, approval, and truthful audit. It also
buys an obligation that a level-triggered reconciler never has, and this design had not paid it.

Kubernetes has no Plan, so a controller simply looks at the newest desired state on every pass and
the question does not arise. Korpis persists the plan, which means a plan can be **half applied**
when a newer intent arrives. `Plan.status` covers `superseded`, but supersession as written is
about the moment of computation: a plan is invalid if reality moved under it. It says nothing about
a plan that is `applying`, whose agent is on step two of five, when someone clicks stop.

The compare-and-swap that guards the mutation is on `intent_version`. It is not on "no plan is
currently applying", so the new intent commits, a second plan is computed, and it is computed
against a reality that is *half of the first plan*, which is a state the model has no way to name.

Two things close it, and both are needed:

**Plans are serialized per workload.** A workload has at most one plan in `applying` at a time,
enforced by the store rather than by convention. A plan proposed while another is applying is
computed and persisted as normal, so it is inspectable, and it enters `queued`.

**A queued plan preempts rather than waits, when it can.** Waiting for a five-step migration to
finish before honouring a stop button is not acceptable, so the applying plan can be cancelled, and
cancelling is a change to desired state rather than a message that must arrive:

```
control plane: plan.cancel(plan_id)
  → plan.status = cancelled, and the plan leaves every assignment that carries it, which
    lowers its `through` below every step not yet started (§4.8 of 02-architecture.md)
  → control-plane and extension steps not yet claimed are withdrawn from the queue
agent: reads its assignment before each step; finishes the step in flight, begins no other
  ──effect: step N completed──▶ control plane: plan.completed = N, nothing else moves
resources from steps 1..N retained and named; the targets stay where approval moved them
```

> Corrected 2026-10-13, from the implementation (`crates/korpis-agent/tests/agent/reconcile.rs`
> holds a step in flight, cancels, and checks the next is never begun). The plan is marked
> cancelled at once rather than after the node's report, because a node with no step in flight
> sends no report and the plan would never have become cancelled; a completion that arrives
> afterwards still counts toward `completed`. A begun plan's targets stay where its approval moved
> them, as after a failure: the steps it ran are the state the next plan is computed against.

> Second external review, finding S1. The first version sent `plan.cancel` to the agent as a
> message, which the agent protocol did not contain and which, being edge-triggered, could be lost
> exactly when it mattered. Lowering `through` is level-triggered: a lost update is repaired by the
> next sync like any other. The first version also set "workload state = partial", which is a fact
> about the plan and was being written into a field only the agent may write; it is now
> `plan.completed`, held by the control plane, and the interface derives the workload's displayed
> state from it.

Cancellation lands **on a step boundary**, never inside one, which is what makes it cheap: §3.1 of
`05-scheduling.md` already requires every step to be ordered, idempotent, and resumable from the
last completed one, and a cancelled plan is a partial plan the model can already express. The next
plan is computed against that partial state exactly as it would be against a failed one.

**A plan cancelled before any of its steps completed leaves the workload as it was.** Approval
moved the target (or the power target) when the plan was approved, not when it ran, so cancelling
an approved or queued plan whose `completed` is still empty puts the targets back to the plan's
`from_intent` and `from_power` by compare-and-swap, and the next sync tells the nodes nothing new.
Only the plan that moved the targets last can do that; one that a later approval has overtaken is
refused with a conflict rather than undoing the later one. (Added 2026-10-13, from the
implementation: the steps above describe an applying plan and said nothing about one approved but
not yet begun.)

Cancellation is a request, not a command. A step in flight completes, because interrupting a
half-written volume is how a system acquires states nobody declared. The cost is a bounded wait
equal to one step, which is stated, rather than an unbounded wait equal to one plan, which was the
alternative nobody had chosen on purpose.

---

## 8. Backup and restore of the control plane

The store is the only stateful component in the control plane, so backing up Korpis is backing up
one PostgreSQL database. Continuous WAL archiving with point-in-time recovery.

**Restoring a store while agents are running is dangerous, and the danger is leases.**

A restored store contains lease epochs from the moment of the backup. Agents in the field hold
*newer* epochs. Under the fencing rule of §4.5 of `02-architecture.md`, an agent presenting an
older epoch is fenced, the restored control plane would consider every running agent stale, while
those agents consider themselves authoritative. That is precisely the split brain leases exist to
prevent, reintroduced by the recovery procedure.

**Restore therefore includes a mandatory epoch fence.** Getting the arithmetic of that fence right
took a correction, and the wrong version is worth leaving visible because it is an easy mistake to
repeat.

> External review, finding R11. Recorded in §16 of `23-walkthroughs.md`.

The first version of this section said the store records a `max_issued_epoch` watermark "so the
restore has a concrete number to advance beyond". **That watermark is in the database being
restored**, so it holds the value as of the backup, and every epoch issued between the backup and
the failure is by definition larger than it. Advancing past a stale watermark does not clear the
epochs that actually exist in the field. The sentence described an intention, not a mechanism.

Two mechanisms do work, and the restore uses both:

1. **A bounded issue rate makes the watermark usable again.** Epochs are issued at a rate the
   control plane caps and records, so `W_backup + rate × max_backup_age` is a real upper bound on
   what could have been issued, and the restore advances past that. The cap is the reason the bound
   exists; without it the arithmetic has no ceiling.
2. **Reconnecting agents raise the floor.** The restored control plane refuses to issue for a
   configured settling window, during which every agent that reconnects presents the epoch it
   holds, and the control plane jumps past the maximum it saw. This closes the case where 1's bound
   was computed against a wrong backup age.

The adversarial corner of 2 is that an agent reports the epoch, so a compromised or broken agent
can report an enormous one and exhaust the epoch space. **Epochs are signed by the control plane
that issued them**, and an agent presenting an epoch it cannot prove was issued is fenced rather
than believed. An epoch is authority, and this design does not take an unverified claim of
authority from anywhere else either.

Mechanism 1 alone is safe but conservative. Mechanism 2 alone is exact but depends on agents
appearing. Together the fence is correct when every agent reconnects and still correct when none
does.

Agents reconnect, discover their epochs are stale, and **freeze mutations while leaving processes
running**, then request reissue according to their `on_expiry` policy.

> External review, finding R12.

Fail-static rather than fail-stop, and the default matters because the alternative is a recovery
procedure that stops tenants' workloads. Fencing exists to prevent two authorities mutating one
workload. A fenced agent already refuses mutations, so killing the processes it is supervising
prevents no inconsistency that refusing mutations has not already prevented. It only converts a
control-plane incident into a tenant-visible outage, which is the opposite of what the restore is
for. `on_expiry: keep_running` is therefore the derived default for every workload whose storage no
other node can attach (§4.5 of `02-architecture.md`), and `stop` remains the default for the
workloads whose correctness genuinely depends on single ownership of a shared resource.

The system converges from a known state instead of a contested one. Slower than pretending nothing
happened; correct.

**And restore recovers the declarations it lost, from the nodes that are running them.**

> Second external review, finding S9. Recorded in §17 of `23-walkthroughs.md`.

The two fences above protect authority. They do nothing for **configuration**, and a restore loses
some. Restore to Tuesday's backup when Thursday's agents are running intents declared on
Wednesday:

- The store's head for a workload is version 5; its agent is running version 7. A full resync sends
  version 5, and the workload is silently rolled back to Tuesday's configuration with no Plan and
  no record, which is P5 and P9 broken by the recovery procedure.
- The next declaration is numbered 6, a version that already existed with a different body. A
  label or step key built from the version number would conclude it had already been applied.
- A workload created on Wednesday has no row at all, so its agent's runtime objects become orphans.

So the settling window that raises the epoch floor does a second job. Every assignment a node holds
is signed by the control plane that issued it (§4.8 of `02-architecture.md`) and carries the full
target intent, its owner, and its power state. During the window, each reconnecting agent presents
its assignments, and for every one whose signature verifies against the control plane's
assignment-signing keyring:

| The restored store lacks | It records |
|---|---|
| the target intent | a **recovered** intent with its original identifier, version, body, and declarer, as signed |
| the workload | a recovered workload under the signed owner, with that intent as head and target |
| the power version | the recovered power declaration |
| the plan the node is executing | a plan in `applying`, marked recovered, whose approval record is lost and says so |

Every recovered row is written with an effect naming the node that presented it and the signature
that vouched for it, and each workload's version counter is advanced past the highest version any
node presented, so no version number is ever issued twice. Runtime objects are labelled with intent
**identifiers**, never versions (§4.4 of `02-architecture.md`), so even a collision that slipped
through could not make a new declaration look already applied. An assignment whose signature does
not verify is not recovered; its workload is fenced like any other unaccounted-for authority.

**Lost effects are re-ingested from the nodes, within a stated horizon.** Agents keep acknowledged
journal records for a retention window after acknowledgement (default seven days, §4.10 of
`02-architecture.md`). The restored cursor is the backup's, so a reconnecting agent resends
everything after it and the effect log refills itself, exactly once, in order. A backup older than
the retention window leaves a gap, and the gap is recorded as one: the agent answers a cursor older
than its journal's tail with a gap record standing for the numbers it no longer holds (§4.10 of
`02-architecture.md`).

**What cannot be recovered, stated plainly.** A declaration that was made after the backup and never
approved reached no node, so no node can present it; it is lost. Control-plane effects with no node
counterpart (an approval, a grant issued and never used) between the backup and the failure are
lost, and the restore writes a single gap record covering that interval so the audit log says what
it does not know. Grants issued in that interval are already void, because the signing key rotates
(below). The interval is as long as the backup is old, which is why continuous WAL archiving is the
documented configuration rather than nightly dumps.

**And restore rotates the signing key, for the same reason.**

> Finding 9 of `23-walkthroughs.md`.

Leases are not the only authority that outlives the database. §6 of `08-identity.md` makes a grant
and its capability token the same object in two representations, and the agent verifies tokens
**offline** against a signing key, which is what lets console access survive a control-plane
outage, and which has a consequence nobody wrote down until a walkthrough went looking for it.

Restore to Tuesday's backup. A grant issued Wednesday and revoked Thursday is absent from the
restored database, but its token is signed by a key that was restored along with it, so it still
verifies. **A revoked grant is silently resurrected, and no row anywhere records that it exists.**
The audit trail is not merely incomplete; it contradicts reality. The symmetric case is milder and
also wrong: a legitimate grant issued Wednesday keeps working while the interface shows nothing, so
nobody can revoke it.

So restore rotates the signing key. Every token issued after the backup stops verifying, and every
session re-authenticates. **A restored control plane must invalidate authority it can no longer
account for**, the same principle as the epoch fence, applied to the other kind of authority the
system hands out.

The cost is a bounded, visible interruption during a disaster the operator is already handling. The
alternative is authority in circulation with no record of it, which is the worse outcome by a wide
margin, and the token lifetimes of §6 of `08-identity.md` bound how long the disruption lasts.

**Key rotation and epoch advancement are both unskippable restore steps.** Neither is a documented
procedure someone can forget; both are performed before the restored control plane serves a
request.

---

## 9. Scale expectations

Stated so they can be measured against, and so the point at which this design must be revisited is
known in advance rather than discovered in production.

| Dimension | Comfortable | Revisit at |
|---|---|---|
| Nodes | 1 – 500 | 1,000 |
| Workloads | 1 – 50,000 | 100,000 |
| Intents/second | < 10 | 100 |
| Effects/day | millions | tens of millions |
| Observation history | thousands of samples/second | on the nodes, outside the store by design (§5) |
| Latest-observation writes | one per state transition | proportional to churn, not to workload count |
| Node heartbeats | one per node per 30 s: about 17/s at 500 nodes | the interval, before the node count |
| Metering rows | one per workload per interval: about 14/s at 50,000 workloads, hourly | the interval (§2 of `15-observability.md`) |

The load that scales fastest is observation history, and §5 keeps it out of PostgreSQL entirely.
What remains scales with *decisions* and with three steady streams that are counted here rather than
rounded to zero: transitions, node heartbeats, and metering. A node running two hundred workloads
that nobody touches costs the store one heartbeat write per interval and two hundred metering rows
per metering interval, a few writes a minute.

> Second external review, finding S11. This paragraph said such a node generated "approximately
> zero transactional load", while the latest observation was being updated in place in PostgreSQL
> and metering, which `15-observability.md` places in PostgreSQL, grows with workloads times
> intervals. Both are small. Neither is zero, and a figure someone will measure against should be
> the real one.

---

## 10. Open questions

1. **Which time-series engine? Resolved in §5: none.** Embedded (a local columnar store, keeping
   the single-binary promise) versus external (Prometheus/VictoriaMetrics/TimescaleDB, better
   tooling, another dependency). This interacts with §5 and with the metering guarantees, since
   metering must not be lossy. It was routed to `15-observability.md`, which never took it up
   (finding S18). Observation history is a `metric` stream on the node, metering is in PostgreSQL,
   and fleet dashboards are the operator's own system fed by Prometheus and OTLP export.
2. **Retention defaults for intents.** Intents are small and are the rollback history, so keeping
   them forever is affordable, until a workload edited by an autoscaler accumulates millions.
   Pruning by age loses old rollback targets; pruning by count loses the oldest. Neither is
   obviously right. → here, once autoscaling behaviour is designed
3. **Is `Plan` worth persisting after it is applied?** Applied plans are referenced by effects and
   are the record of what was decided, but they are large and most are auto-approved non-events.
   Possibly: persist plans that required approval or contained irreversible steps, and summarize
   the rest into their effects. → here
4. **Archive query path. Resolved here.** Detached effect partitions in object storage must stay
   queryable for incident review and compliance. Is that a Korpis feature, or an exported open
   format and somebody else's query engine? The second is cheaper and matches "Korpis is not a
   monitoring stack" (§2 of `00-overview.md`). It was routed to `15-observability.md`, which never
   took it up (finding S18).

Archived partitions are written as Apache Parquet with a published schema, alongside the day's hash
chain and its signed root (§4 of `15-observability.md`), so an archive is verifiable without
Korpis running and queryable by any engine that reads Parquet. Korpis ships `korpis audit verify`,
which checks a chain against its published roots, and does not ship a query engine. An archive that
can only be read by the system that wrote it is not evidence; one that any tool can read and anyone
can verify is.
5. **Control plane read scaling.** Read replicas serve the dashboards well, but replication lag
   makes a client see its own write disappear, the worst possible UX for an operator who just
   pressed a button. Read-your-writes routing is the standard fix and it needs designing rather
   than assuming. **Resolved in §6 of `10-api.md`**, a `consistency_token` returned by every write
   and replayed on reads.
6. **Multi-region writes.** One PostgreSQL primary and nodes on three continents means every
   declaration and every journal ingestion crosses an ocean. Regional read replicas and the
   `consistency_token` of §6 of `10-api.md` cover reads. Whether writes need regional partitioning
   is a large change if so, and it touches the epoch watermark of §8, which assumes one issuer.
   Routed here from §10 of `18-operations.md` (question 2) and not carried until now (finding S18).
   → here
