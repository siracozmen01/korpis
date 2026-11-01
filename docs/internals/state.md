# State

How intent, plans and effects are stored, why there is no event sourcing, and how plans for one
workload are kept from colliding. Restoring the control plane is on
[control plane restore](control-plane-restore.md).

## Contents

- [No event sourcing](#no-event-sourcing)
- [Four kinds of data](#four-kinds-of-data)
- [Intents](#intents)
- [Declaring and approving](#declaring-and-approving)
- [Authorization is not in the database](#authorization-is-not-in-the-database)
- [The effect log](#the-effect-log)
- [Telemetry](#telemetry)
- [PostgreSQL only, and the job queue](#postgresql-only-and-the-job-queue)
- [One plan at a time](#one-plan-at-a-time)
- [Scale](#scale)

## No event sourcing

The obvious design for intent, plan and effect is full event sourcing, with every queryable state a
projection of the log. Asking what history is actually needed for shows it is not:

| Need | Provided by | Needs event sourcing? |
|---|---|---|
| who did what, under what authority | the effect log | no: an append-only table |
| roll back to an earlier configuration | the intent chain | no: intents are already immutable and versioned |
| explain an incident afterwards | observation history | no: a time series |
| bill for usage | metering | no: a time series |

The intent chain already stores the declared configuration of a workload as its complete history,
so rollback is pointing at version N, not replaying anything. Event sourcing would pay for
projections that drift, rebuilds measured in hours, event schema migration and debugging through
an indirection, to buy what the model already has.

**PostgreSQL is authoritative. Intents are immutable rows, effects an append-only table, with no
projections, rebuilds or replay.** An effect of a control-plane decision (a declaration, an
approval, a grant, a denial) commits in the same transaction as the change it describes, or neither
commits. An effect of something on a node (a restart, a keystroke, a drift correction during an
outage) cannot share a transaction with an event on another machine; it is written first to the
node's durable journal and ingested exactly once, in journal order, in the transaction of the
observation or change it explains. Where a journal had to drop records, the loss is itself a
record. So audit cannot drift from reality, which a separate audit subsystem cannot guarantee.

## Four kinds of data

| Kind | Objects | Volume | Consistency | Where |
|---|---|---|---|---|
| configuration | organizations, projects, quotas, grants, workloads, intents, plans, volumes, endpoints, placements, leases, extensions | small, relational | strong, transactional | PostgreSQL |
| audit | effects | high, append-only | strong at write, archived later | PostgreSQL, partitioned by month, then object storage |
| metering | one row per workload per interval | moderate | exact, idempotent, gap-marked | PostgreSQL, partitioned |
| telemetry | observation history, metrics | very high, little value per row | best effort | on the nodes, never the main database |

A thousand workloads reporting every few seconds is millions of nearly worthless rows a day; put in
the store that holds grants, it dominates a transactional store with traffic that never needed a
transaction. Pterodactyl avoids this by keeping no observations at all, so no installation can say
what a server was doing at 3am on Tuesday. Korpis keeps them, elsewhere.

## Intents

```sql
CREATE TABLE intent (
  id uuid PRIMARY KEY, workload_id uuid NOT NULL REFERENCES workload(id),
  version bigint NOT NULL, parent_id uuid REFERENCES intent(id),
  declared_by text NOT NULL, declared_at timestamptz NOT NULL DEFAULT now(),
  body jsonb NOT NULL, UNIQUE (workload_id, version));
REVOKE UPDATE, DELETE ON intent FROM korpis_app;
```

Immutability is enforced by the database, because application code is where such invariants
eventually get violated. A revocation binds only a credential that cannot take it back, so the
running control plane connects as `korpis_server`, a login that is a member of `korpis_app` alone;
the schema owner's credential is used to migrate and nothing else. On PostgreSQL 16 and later,
creating a role no longer makes its creator a member, so a migration grants the owner `korpis_app`
with `SET` and without `INHERIT`.

`body` is `jsonb` because it evolves with every driver, endpoint type and probe, and it is
validated against the schema before insertion rather than by column types. It holds the
`korpis.v1.IntentBody` protobuf message in proto3 canonical JSON: one representation, whose schema
is the `.proto`, queryable in PostgreSQL and round-tripping exactly. Intents written years ago must
stay readable, because rollback re-declares an old version, so:

- **Field names, not numbers.** The wire rules already forbid renaming a field, which protects JSON
  keys for free.
- **Rows are never rewritten**, so a field a later version removed stays in old rows and stays
  reserved forever.
- **The schema version is recorded on the intent**, so an old intent is parsed with the schema in
  force when it was written.
- **Defaults are written, not omitted.** Proto3 JSON omits default values, so a stored body missing
  a field would mean whatever the default is when read, and changing a default would silently
  change every old row. The API's rule that a field's default never changes defends the same
  property from the other side.
- **Integers are strings** in canonical JSON, so the few fields indexed or range-queried (byte
  counts, epochs, versions) have the cast written into the index definition.

## Declaring and approving

Two clients declaring for one workload at once is normal. Last-write-wins would silently discard
one, so every declaration is a compare-and-swap on the head of the chain:

```sql
UPDATE workload SET intent_id = $new, intent_version = $expected + 1
 WHERE id = $workload AND intent_version = $expected;
```

Zero rows means someone moved first; the loser gets a conflict carrying the winner and recomputes
its plan. A plan is never applied against a state it was not computed for.

**Declaring changes no node. Approving does.** The head is what was last declared; the target is
what was last approved, and only the target enters a node's assignment. Approval is a second
compare-and-swap, on the target, permitted only when the plan's intent is the head, committed with
the plan's move to approved or queued and the effect naming the approver. Power has the same two
pointers, its head and its approved target.

## Authorization is not in the database

Row-level security fits the tenancy model and is deliberately not used. Grants are evaluated in the
gateway and nowhere else; a second copy of the logic in database policies would make two places
that decide authority, which eventually disagree, and the disagreement is an invisible security bug.
The database enforces referential integrity and immutability, never policy.

## The effect log

```sql
CREATE TABLE effect (
  id uuid NOT NULL, at timestamptz NOT NULL, occurred_at timestamptz,
  plan_id uuid, step int, workload_id uuid, node_id uuid,
  actor text NOT NULL, grant_id uuid, action text NOT NULL, outcome text NOT NULL,
  error jsonb, before jsonb, after jsonb,
  origin_node uuid, origin_seq bigint,
  PRIMARY KEY (at, id)) PARTITION BY RANGE (at);
REVOKE UPDATE, DELETE ON effect FROM korpis_app;
```

- **Partitioned monthly on the ingest clock.** `at` is when the control plane ingested the effect,
  from one clock, and is the key and partition boundary. A node's own reading is `occurred_at`,
  never a key: a node clock minutes fast would write into a partition that does not exist, and
  causal order would invert in the one log meant to be beyond argument. A large gap between the two
  is itself worth an alert. Detaching a partition is instant; deleting millions of rows is not.
- **Archived, never deleted.** Detached partitions are written as Apache Parquet with a published
  schema, beside the day's hash chain and its signed root, so an archive can be verified and queried
  by any engine without Korpis running. Korpis ships `korpis audit verify`, not a query engine. An
  archive only its writer can read is not evidence. Retention governs what is online, not what
  exists.
- **Every effect names its grant**, not just its actor, because "under what authority, delegated
  from whom" is the question an incident needs. The control plane's own effects name the grant of
  the decision they carry out; a denial names its subject and the missing action.
- **Drift correction writes effects too.** Restarts and repairs produce no plan; the plan log says
  what someone decided, the effect log what actually happened.
- **Node effects carry their journal position.** A per-node cursor, `node_journal_cursor`, advanced
  in the ingesting transaction, makes ingestion exactly-once without a unique index, which
  PostgreSQL cannot enforce on a partitioned table without the partition key.

## Telemetry

**The latest observation** is one row per workload in PostgreSQL, updated in place: it is "now", not
history. It is written on every state transition, not on a timer; between transitions, a node's
heartbeat keeps it current.

**Observation history is a `metric` stream on the node**, owned by the agent like every stream:

```
raw         full fidelity   hours
1 minute    downsampled     weeks
5 minutes   downsampled     months
1 hour      downsampled     years
```

It is written and downsampled on the node, migrates with the workload, is offloaded with its other
streams, and is read through the relay like console scrollback; "what was it doing at 3am" is a
seek. That keeps three promises a separate time-series engine would break one of: the control plane
stays stateless, a single machine needs nothing more installed, and history follows the workload
rather than the node it ran on. Fleet dashboards are the operator's own, fed by Prometheus and OTLP
export.

State transitions (`running` to `crashed`, `healthy` to `unhealthy`) are never downsampled: they are
promoted into the effect log, because a crash is an event. Downsampling loses the CPU curve, not the
fact that something died.

**Metering is not telemetry.** It is the one measured series with financial consequences, so it is
never downsampled, dropped or rebuilt from averages: one row per workload per interval in
PostgreSQL, with the transactions, idempotent re-delivery and period closing an invoice needs. See
[observability](observability.md).

## PostgreSQL only, and the job queue

**PostgreSQL is the only supported store; SQLite, MySQL and MariaDB are not.** `SKIP LOCKED` with
`LISTEN/NOTIFY` makes a correct, fast, dependency-free queue; an abstraction that also runs on
SQLite degrades to polling, and the degraded path is the under-tested one where the bugs live.
Several engines multiply the test matrix forever (Pelican carries three code paths for every query),
and a portable subset would give up `jsonb`, partial and expression indexes, partitioning and
transactional DDL. The cost is that an operator runs PostgreSQL, which the installer provides, so a
single operator on one machine still installs one thing.

```sql
CREATE TABLE job (
  id uuid PRIMARY KEY, key text UNIQUE NOT NULL, kind text NOT NULL, payload jsonb NOT NULL,
  run_after timestamptz NOT NULL DEFAULT now(), attempts int NOT NULL DEFAULT 0,
  max_attempts int NOT NULL DEFAULT 10, locked_by text, locked_until timestamptz,
  status text NOT NULL);  -- pending, running, succeeded, failed, cancelled
```

A claim is one statement: the oldest due job of the kinds a worker takes, locked with `SKIP LOCKED`,
marked running under the worker until its lease runs out, its attempt counted. A job whose lease ran
out is due again, so a dead replica's work is taken by another, and the first holder can no longer
end it. A job is queued in the transaction that decides it is due and ended in the transaction that
records what it did. Queueing notifies `korpis_job`, and workers also look every five seconds,
because retries come due by the clock. The key of a plan step is the plan and step index, so queuing
the same work twice is a no-op. A job that cannot begin for a reason with a known end (a sweep
waiting for a writer's access to lapse) is deferred without spending an attempt. No Redis, NATS or
broker until a measurement demands one.

## One plan at a time

Persisting plans means a plan can be half applied when a newer intent arrives: the agent is on step
two of five when someone clicks stop. The compare-and-swap on the intent version does not stop a
second plan being computed against half of the first, a state the model cannot name. Two rules close
it.

**Plans are serialized per workload.** At most one plan per workload is applying, enforced by the
store. A plan proposed meanwhile is computed and persisted, so it can be inspected, and is queued.

**A queued plan preempts when it can.** Waiting for a five-step migration to honor a stop button is
not acceptable, so the applying plan can be cancelled, and cancelling is a change to desired state,
not a message that must arrive:

```
control plane: plan.cancel
  -> the plan is cancelled at once and leaves every assignment carrying it,
     lowering how far its node may go below every step not yet started
  -> control-plane and extension steps not yet claimed are withdrawn
agent: reads its assignment before each step; finishes the step in flight, begins no other
  -> its completion still counts toward the plan's progress
```

A lost update is repaired by the next sync, because lowering how far a node may go is
level-triggered. Cancellation lands on a step boundary: every step is ordered, idempotent and
resumable, and a cancelled plan is a partial plan the next one is computed against, as after a
failure. A step in flight completes, because interrupting a half-written volume is how a system
acquires states nobody declared; the cost is a wait of one step, stated, instead of one plan. A plan
cancelled before any step completed leaves the workload as it was: approval had moved the targets,
so cancelling puts them back by compare-and-swap, and only the plan that moved them last may; one a
later approval overtook is refused rather than undoing the later one. The workload's displayed
state is derived from its plan's progress, which the control plane holds; the agent alone writes
observations.

## Scale

Stated so the point to revisit the design is known in advance.

| Dimension | Comfortable | Revisit at |
|---|---|---|
| nodes | 1 to 500 | 1,000 |
| workloads | 1 to 50,000 | 100,000 |
| intents per second | under 10 | 100 |
| effects per day | millions | tens of millions |
| observation history | thousands of samples a second | on the nodes, by design |
| latest-observation writes | one per state transition | proportional to churn |
| node heartbeats | one per node per 30 s, about 17/s at 500 nodes | the interval first |
| metering rows | one per workload per interval, about 14/s at 50,000 hourly | the interval |

What reaches the store scales with decisions and three steady streams counted here rather than
rounded to zero: transitions, heartbeats and metering. A node running two hundred untouched
workloads costs one heartbeat per interval and two hundred metering rows per metering interval.

Read replicas serve dashboards; every write returns a consistency token that a later read replays,
so an operator never sees their own write disappear behind replication lag.
