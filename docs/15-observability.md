# Metrics, Metering, Audit, and Alerting

**Status:** design **Date:** 2026-08-07 **Depends on:** [`03-state.md`](./03-state.md),
[`14-streams.md`](./14-streams.md), [`06-storage.md`](./06-storage.md) **Implements:** Principles
P4, P5; Rules K-3, K-12, K-16

---

## 1. Four things, not one

Panels in this market build one subsystem called "stats" and use it for everything. That is why
their billing numbers are wrong: a series designed to draw a graph is sampled, lossy, and
interpolated, which is correct for a graph and unacceptable for an invoice.

| | Consumer | Guarantee | Retention | Store |
|---|---|---|---|---|
| **Metrics** | operators, dashboards | best-effort, lossy, gap-tolerant | short at full resolution | time-series, outside Postgres |
| **Metering** | billing systems | **exact, gap-marked, idempotent** | years | Postgres, partitioned |
| **Audit** | operators, tenants, compliance | append-only, attributable, tamper-evident | years | Postgres, `Effect` |
| **Alerting** | humans | delivered or visibly not | none, it is a routing function | none |

They have different guarantees, so they are different systems. Collapsing them is a decision that
cannot be undone later, because by then invoices depend on it.

---

## 2. Metering, because money depends on it

### Measured, never assumed

Every metered quantity comes from a kernel counter, cgroup v2 accounting, `statfs`, nftables
counters, driver-reported device usage. **No metered quantity is ever derived from a configured
limit.** Pterodactyl reports the limit you set as though it were consumption because it cannot
measure the difference; that is the failure P4 and K-3 exist to prevent, and it is worst here,
because the number becomes an invoice.

### What is metered

```
cpu_seconds            cumulative, per workload
memory_byte_seconds    the integral, not the peak, peak is reported separately
disk_bytes_stored      sampled at a declared interval
disk_io_bytes          read and write, separately
network_bytes          ingress and egress, separately, on-net and off-net separately
device_seconds         per device class (GPU and others), from the driver
address_hours          dedicated IPv4 is scarce and is a real cost (§4 of `07-networking.md`)
backup_bytes_stored    post-deduplication, with the dedup scope named (§5.3 of `06-storage.md`)
```

Memory is metered as byte-seconds because that is what capacity actually costs, and peak is
reported alongside it because that is what people argue about. Both are published; which one a
billing system charges on is not Korpis' decision (K-12).

### Gaps are recorded, never interpolated

A missing interval is written as a gap with its cause (node unreachable, agent restarting, counter
reset) and the billing system sees the hole. The alternative, interpolation, means a host silently
bills a customer for a period nobody measured, which is indefensible the first time it is
challenged and is standard practice everywhere else.

Same principle as `14-streams.md` §3, applied where it is most expensive to violate.

### A gap is a claim about knowledge, not about the workload

> Findings 10 and 12 of `23-walkthroughs.md`.

The paragraph above is written from the control plane's side, where a gap means *no samples
arrived*. The agent's side is different: it kept measuring from kernel counters the whole time,
because a node that loses its uplink has not lost its cgroups. **A gap in delivery is not a gap in
measurement**, and treating the first as the second discards consumption that was measured, which
is the interpolation error inverted.

So a gap carries the same identity as any other record, `(workload, resource, interval_start)`, and
a real sample for that interval **supersedes it**. The control plane writes a gap when it stops
hearing, and retracts it when it hears. The retraction is itself dated and visible, because a
number that changed is a thing a host may have to explain.

There is exactly one case where a gap is a statement about the workload rather than about the
control plane, and it is the case the agent creates deliberately:

**The agent's metering buffer is bounded, and the bound is declared.** `14-streams.md` §5 bounds
the console and says what a drop looks like; metering, which is the one stream with financial
consequence, gets the same treatment rather than an assumption that the buffer is large enough.
The buffer is the node's journal (§4.10 of `02-architecture.md`), shared with effects, so there is
one bound, one horizon reported in `NodeStatus`, and one place an operator raises it. Exhausting it
writes a gap with the cause `journal exhausted`, which no later delivery can supersede because there
is nothing to deliver. The trade is stated: local disk against how long a node may be partitioned
before its meter has holes in it.

### Periods close, or the numbers never stop moving

> Finding 11 of `23-walkthroughs.md`.

K-12 keeps Korpis out of billing and that is still right. It does not excuse handing a billing
system a series with no notion of finality, which is a moving target rather than an input.

```
MeteringPeriod
  boundary        the period's own start and end
  lateness_window Duration     how long after the end a sample may still revise it
  state           open | closing | closed
  closed_at       Timestamp
```

A sample arriving inside the lateness window revises an open period. A sample arriving after it is
recorded in the current period as a **dated adjustment naming the interval it belongs to**, never
as a silent edit of a closed one. A host that has already invoiced can therefore reconcile, and a
host that has not can wait for `closed`.

The lateness window is the same kind of number as a token lifetime in §6 of `08-identity.md`: it
bounds how long the past can change, it is stated rather than convenient, and an operator picks it
against a real trade. A long window tolerates partitioned nodes and delays every invoice. A short
one bills on time and sends more adjustments.

### Idempotent by construction

A record's identity is `(workload, interval_start)`, every resource a column. Re-delivery of the
same journal record is dropped by the journal cursor before it reaches the table (§4.10 of
`02-architecture.md`), so an agent that reconnects and re-sends an hour of buffered records cannot
double-bill anyone, which matters because that reconnect is a normal event, not an exceptional one.

**Changing a number appends; it never overwrites.** A real sample that replaces a gap, a late
sample inside the lateness window, and a correction all insert a new version of the record with a
`supersedes` reference to the one it replaces, and the current value is the newest version. The
table is append-only like the effect log, so "what did this interval say on the day it was
invoiced" is a query, not a reconstruction.

> Second external review, finding S11. This paragraph said re-delivery "overwrites", while §5 of
> `03-state.md` called metering append-only and this section's own gap retraction already needed the
> earlier value to stay visible. Supersession satisfies all three.

### Continuity across restart, reboot, and migration

Counters are cumulative **per workload**, not per process, per container, or per node. A workload
that restarts does not reset its meter; a node that reboots does not lose the interval; a migration
does not double-count and does not drop.

Migration is the hard one, and it is already solved: the seven-phase migration of §8 of
`05-scheduling.md` has exactly one instant at which ownership transfers, fenced by a lease epoch.
The source node's final sample and the destination's first are cut at that instant. Without a
fenced cutover this is unsolvable, which is a good illustration of why the fencing was designed
first.

---

## 3. Metrics

Prometheus exposition and OTLP. No custom protocol, no proprietary agent, no bundled dashboard
product, §2 of `00-overview.md` is explicit that Korpis is not a monitoring stack.

Three scrape surfaces:

- the control plane's own metrics: reconciliation lag, plan latency, queue depth, API rates
- each agent's metrics: node capacity, driver health, convergence lag per workload
- **a per-tenant endpoint, scoped by grant**: a tenant scrapes their own workloads into their own
  Prometheus, seeing exactly what their grants permit and nothing about the node or its neighbours

That third one does not exist in any competitor and costs nothing here, because the authorization
model already answers "what may this subject see" for every other read.

**Cardinality is a hard constraint, not a guideline.** Workload identifiers are bounded and are
labels. Player names, file paths, remote addresses, and anything else a tenant controls are not,
and never become labels, a tenant able to inject unbounded label values can take down the
operator's monitoring, which makes it an availability vulnerability rather than a tidiness concern.

Storage: recent data at full resolution on the node, as the workload's `metric` stream, downsampled
there and migrated and offloaded with it (§5 of `03-state.md`), and exported in Prometheus and OTLP
form to whatever the operator already runs for anything fleet-wide. §5 of `03-state.md` keeps
telemetry out of the transactional store; metering is the deliberate exception, because its volume
is low and its value is high.

---

## 4. Audit is not a separate system

The `Effect` log **is** the audit log. There is no second table, no separate writer, and therefore
no possibility of the two disagreeing.

This falls out of §1 and §4 of `03-state.md`: an `Effect` is append-only and names the grant that
authorized it (a denial, which nothing authorized, names its subject and the missing action, as
below). An effect of a control-plane decision is written in the same transaction as the
decision; an effect of something that happened on a node is ingested exactly once, in order, from
that node's journal, in the same transaction as the state it explains. Nothing can change state
without producing one, because producing one is how state changes. Bolt-on audit logs are
incomplete in exactly the cases that matter, since the paths that skip them are the unusual ones.

Three properties worth naming explicitly:

**Denials are recorded.** Most systems log what happened; the interesting security signal is what
was attempted and refused. A denied action produces an `Effect` with its outcome, its subject, and
the action that was missing. Repeated identical denials within a window are one effect with a count,
and denials are charged against the caller's rate limits (§10 of `10-api.md`), so recording them
cannot be turned into an unbounded write path.

**Sensitive reads are recorded.** Reading a secret config field, downloading a backup, attaching to
another tenant's console under operator authority (§7 of `08-identity.md`), these are events. An
audit log that only contains writes cannot answer "did anyone look at this".

**Tamper-evidence, not just append-only.** `REVOKE UPDATE, DELETE` stops the application; it does
not stop someone with database access. Each day's effects are hash-chained and the day's root is
signed and published. An operator who edits history can still do so, but no longer silently, which
is the achievable guarantee, and claiming more would violate P4.

Export is to open formats for whatever SIEM the operator runs. Tenants can read their own audit
trail scoped by grant, which is unusual in this market and is simply what happens when the audit
log is the same object the authorization model already filters.

---

## 5. Alerting stops at the boundary

Korpis emits **events** with severity, subject, and structured detail. It does not build alert
routing, deduplication, escalation policies, on-call schedules, or silences. Alertmanager,
PagerDuty, and their peers exist and are better at it.

The minimum that ships, because it is Bet 1 rather than a monitoring feature:

```
selector: project "community-servers", severity >= warning
deliver:  discord channel #ops-alerts
```

A selector, a severity, and a destination. The destination holds a grant and receives only what
that grant permits (§7 of `12-surface-discord.md`). Anything richer (time windows, dependency
suppression, escalation) is an extension or an external system, and the event stream is published
so that either can consume it.

**Health is application-level** (K-16). A process that is running is not a healthy Minecraft
server; a healthy Minecraft server answers a query on its port. Health checks are declared per
workload and defined per tier in §5 of `04-runtimes.md`, and it is health (not process liveness)
that generates events.

### The event log

> Added 2026-10-13, while designing the extension contract of Phase 1. "The event stream is
> published", and §5 of `16-extensions.md` promised its consumers per-workload order and
> at-least-once delivery, but nothing said what an event is, where it is written, or what a
> consumer's position in it is.

```
Event
  id            EventID        global, increasing; a consumer's position is the last it acked
  key           workload:<id> | organization:<id>    what it concerns, and so what orders it
  seq           int            per key, from 1, with no gaps: a consumer sees a hole as a hole
  at            Timestamp      the ingest clock, as for an effect
  kind          string         an effect's action, or a condition: `health.changed`,
                               `convergence.changed`, `provider.degraded`, ...
  severity      info | warning | critical
  organization  OrgID
  workload      WorkloadID?
  effect        EffectID?      for an event that reports an effect
  detail        map[str]Value
```

**Every event is written in the transaction of the fact it reports.** Every effect is an event of
its own action, so the log of what happened and the stream of what to react to cannot disagree,
which is §4's reason for not having a second audit system applied to delivery. A condition (a
workload's health changing, a provider becoming slow) is written where the control plane decides
it. An event is never written afterwards by a process that might not run.

**A key's events are numbered in commit order.** Writing an event takes the key's counter row, so
two transactions about one workload are numbered in the order they commit, and an event's `id`
follows the same order. A consumer is handed an event only once every transaction that could still
commit an earlier `id` has ended, which the control plane reads from PostgreSQL's snapshot horizon,
so advancing past an `id` never skips one that commits late.

> Corrected 2026-10-13, when the log was built. As written, the paragraph above assigned `id` from
> a sequence at insert and delivered below the horizon. That is not enough: a transaction's
> identifier is assigned at its first write and an event's `id` at its insert, so the two orders
> can disagree, and a consumer resuming after `id` N could pass over an event with a smaller `id`
> whose transaction was still open when N was delivered. What is built instead: an event is
> inserted with an internal insertion number, and a **sequencer** gives it its `id`, a dense
> position, once its transaction and every older one have ended (below the snapshot horizon),
> in batches ordered by insertion number. Within a batch that order agrees with every key's
> numbering, because a key's number is taken under its counter row before the insertion number;
> across batches it agrees too, because a later batch's transaction took the key's row only after
> the earlier one committed. So delivering by `id` never hands out a key's events out of order
> and never passes over one. The cost is a delay of one sequencer pass (it is woken by every
> event's commit, and looks once a second regardless), and one sequencer at a time, which several
> control-plane processes take turns being. **The horizon is the whole PostgreSQL cluster's**, not
> this database's: a transaction left open anywhere on the server, in any database, holds every
> event back until it ends. Nothing is lost or reordered, but delivery stalls; §2 of
> `18-operations.md` says what an operator does about it. Narrowing the horizon to this database
> would mean reading other sessions' transaction identifiers from `pg_stat_activity`, which hides
> them from an unprivileged role, and a writer the control plane cannot see would be skipped.

**Where conditions are decided** (2026-10-13, from the implementation). `health.changed` is
written in the transaction that records the observation it differs in, from the node's previous
observation. `convergence.changed` has no such transaction: convergence is derived when it is
read, from the target, the plans, the observation, and the clock, and an observation going stale
changes it without any write. So the control plane decides it on every workload change it is
told of and on every resync, records what it decided, and writes the event when the decision
differs from the last one, swapping the record so that of several processes deciding at once,
one writes it. The cost is a recomputation of every workload's convergence each resync, and a
change that only the clock makes (a node falling silent) is reported up to one resync late. The
first decision about a workload is recorded without an event, since nothing changed. Both carry
`from` and `to`, and a change for the worse (`unhealthy`, `failing`) is a warning.

**What an effect's event carries.** Its `kind` is the effect's action, its `detail` the effect's
`after` and `error`, its severity `info` for an effect that succeeded or was skipped and `warning`
otherwise; `critical` is for conditions. It is keyed to the effect's workload when it has one,
and otherwise to the organization that owns the effect's node or its grant's scope. An effect
that concerns no organization at all, which is only the installation's own bootstrap before any
organization exists to read it, is no event: every event is read under `event.read` on an
object, and such an effect has none.

**A subscription is a stored position, not a connection.** It names its subject, a filter (kinds,
a minimum severity, a selector over projects and workload labels), and the last `id` the consumer
acknowledged. Delivery is at-least-once: a consumer that reconnects resumes after its last
acknowledgement and may see again what it saw but did not acknowledge, and the event `id` is the
delivery identifier that makes it idempotent. Only events about objects on which the subject holds
`event.read` (§5 of `10-api.md`) are delivered, judged by its grants as they are when each is sent.

**Whose a subscription is** (2026-10-13, from the implementation). A subscription belongs to the
subject that created it, and only that subject reads, acknowledges, or deletes it; to anyone else
it does not exist. Creating one needs no particular grant, because it holds nothing: what it is
delivered is judged by its subject's `event.read` grants at each send. Creating and deleting one
are effects; acknowledging is not, being a consumer's progress and as frequent as its reads. A
new subscription starts at the end of the log, so it is delivered what happens from then on,
not thirty days of history it did not ask for. An acknowledgement may not pass the newest
positioned event, so a consumer cannot skip what has not been delivered yet. An extension's
subscriptions are created with its installation, held by `extension:<id>`, narrowed to the
target project when it is installed into one, and deleted when it is uninstalled (§4 of
`16-extensions.md`).

**Bounded, and the bound is visible.** The live stream holds a bounded buffer per subscriber; a
consumer that falls behind it is disconnected with `OVERFLOW` (§7 of `10-api.md`) and resumes from
its position, which the log itself holds. The log is kept for a retention window (thirty days by
default, configurable); a subscription whose position falls out of it receives a gap marker naming
the range it lost and continues from the oldest event still kept. Retention never touches
effects, which are the audit record and are archived, never dropped (§4 above).

What it costs: one counter row update per event, which serializes the events of one workload
(never two workloads), and an event row beside every effect, about doubling the write volume of
the audit path. The alternative, deriving the stream from the effect log after the fact, cannot
promise per-workload order across a control plane that writes from more than one process.

---

### Degraded dependencies are events

> Finding 23 of `23-walkthroughs.md`.

A provider extension that fails trips its circuit breaker and its dependents say so (§5 of
`16-extensions.md`). A provider that is merely **slow** produces no error anywhere while consuming
the fleet's reconciliation capacity, so it needs an event of its own: `provider.degraded`, carrying
the provider, its measured latency, and what is queued behind it.

The operator whose creates got slow this morning should not have to infer the cause from a graph.

> Renamed 2026-10-13 from `provider_degraded`, when event kinds became filters: a kind is dotted
> words, so that `provider.` selects every provider event, and the underscore spelling was the
> only one that could not be selected that way.

---

## 6. Korpis phones home only if asked

An open-source project collecting usage telemetry is a trust decision, and the honest form of it
is: **off by default, opt-in, and the exact payload is printable before you enable it.**

```
korpis telemetry show     prints precisely what would be sent
korpis telemetry enable
```

No instance identifier is transmitted unless the operator enables it, nothing about tenants,
workloads, or names is ever included, and the aggregate is published so the project's own data is
visible to the people who supplied it. P10 forbids charging for growth; quietly measuring it would
be a different way of taxing trust.

---

## 7. Open questions

1. **Metering resolution. Resolved here.** Per-minute intervals are ~525,000 rows per workload per
   year; per-hour is 8,760 and loses burst detail that "dedicated vCPU" claims depend on (§5.2 of
   `05-scheduling.md`). High-resolution recent with automatic rollup is likely, and the rollup must
   remain gap-honest.

The interval is per installation, **hourly by default and as fine as one minute**, with the row
count stated at the moment of choosing it. Totals lose nothing at any interval, because the
counters are cumulative (§2): an hour is the exact difference of two readings, not an average of
samples. What a coarse interval loses is shape, so each record also carries the interval's peak
memory and the CPU time the workload spent throttled, which is the number a dispute about
"dedicated" capacity actually turns on. Finer shape lives in the workload's `metric` stream, which
is not billing-grade and does not claim to be.
2. **Restatement.** A metering bug discovered after invoices are issued needs a correction path
   that is auditable rather than an `UPDATE`. Append-only correction records with an explicit
   supersedes link are the shape; the API for it is unspecified. → here
3. **Node-local buffering horizon. Resolved in §4.10 of `02-architecture.md`**: the node journal,
   a declared bound (2 GiB by default), and a horizon each node measures and reports. How long an
   agent buffers metering while the control plane is unreachable determines how long an outage can
   last before revenue data is lost. §4.6 of `02-architecture.md` guarantees workloads keep running;
   it did not yet guarantee they keep being measured.
4. **Trace propagation.** Following a request across declared dependencies (§5 of
   `07-networking.md`) requires injecting context into tenant traffic, which is intrusive and
   possibly not Korpis' place. Carried from §9 of `14-streams.md`. → here
5. **Tenant-visible operator access.** §7 of `08-identity.md` promises tenants can see when an
   operator accessed their workload. Which accesses are exempt, an operator debugging a node-wide
   failure touching every tenant, is a policy question with real privacy weight. **Resolved in §8
   of `17-security.md`**: none are exempt.
