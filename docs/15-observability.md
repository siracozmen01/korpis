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
authorized it. An effect of a control-plane decision is written in the same transaction as the
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

---

### Degraded dependencies are events

> Finding 23 of `23-walkthroughs.md`.

A provider extension that fails trips its circuit breaker and its dependents say so (§5 of
`16-extensions.md`). A provider that is merely **slow** produces no error anywhere while consuming
the fleet's reconciliation capacity, so it needs an event of its own: `provider_degraded`, carrying
the provider, its measured latency, and what is queued behind it.

The operator whose creates got slow this morning should not have to infer the cause from a graph.

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
