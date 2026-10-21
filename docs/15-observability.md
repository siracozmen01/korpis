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

> Corrected 2026-10-21, when metering was designed ("Measuring it, as designed", below). A sample
> carries cumulative counters, so the first sample a node delivers after a loss measures the whole
> span the lost ones covered; what is lost is how it divides among the span's intervals, and the
> peaks within it. The cause `journal exhausted` stays, on a record that spans the intervals.

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

### Measuring it, as designed

Designed 2026-10-21, when metering was built (Phase 2 of §4 of `20-roadmap.md`). Until then the
node journaled no sample, the control plane recorded the kind it could not store as
`journal.unsupported` (§4.10 of `02-architecture.md`), and the driver's `Stats` answered a rate
over the time since it was last asked and the memory in use now, which no interval can be summed
from. This section says what each quantity is read from, by whom, how often, and what the control
plane keeps; the sections above say what that must guarantee.

**Every counter is cumulative per workload, and the node keeps it so.** The driver answers, for
each runtime object, the kernel's own counters of its cgroup since the cgroup was made: CPU time
and the time it was throttled (`usage_usec` and `throttled_usec` of `cpu.stat`, the second the
number question 1 of §7 says a dispute about dedicated capacity turns on), bytes read and written
(`rbytes` and `wbytes` of `io.stat`, summed over devices), the memory high-water mark
(`memory.peak`), and the memory in use now (`memory.current`). A workload outlives its objects: a
restart, a run, and an upgrade each make a new cgroup whose counters begin at zero. So the agent's
meter keeps, for each workload, a base that each source (an object, or the interface below) adds
its final reading to when it ends, and a workload's counter is that base and its current sources'
readings. An object's final reading is taken before the agent removes it, since a cgroup's counters
are readable until then whether its process exited or crashed. A reading lower than the one before
is a source made again without the meter seeing it end: the last reading joins the base, and what
the source used between that reading and its end, which nothing can read any more, is a gap with
the cause `counter reset`. The meter keeps the base and its last reading of every source in a file
of its own, replaced atomically at every reading, so an agent that restarts takes up where it
stopped and nothing is counted twice or lost; a driver that restarts loses nothing, since the
cgroups are the kernel's.

**Memory is integrated on the node, and says so.** The kernel keeps no integral of memory in use.
The meter reads `memory.current` every ten seconds and adds the reading times the time since the
last one, so `memory_byte_seconds` is a sum of measured samples, never of a limit; its error is how
much the memory moved within ten seconds, and the method is published with the number (§8 of
`11-surface-web.md`). An interval's peak is the most memory read within it, and, where an object's
`memory.peak`, the kernel's high-water mark since the object was made, rose since the reading
before, that mark: so a spike between two readings that set a new high is caught, one below an
earlier high is not, and a spike between an interval's last reading and the next interval's first
is counted in the next. The high-water mark alone would not do, since it is an object's whole
life's and an interval's peak would never fall.

**Network bytes are the kernel's counters of the workload's own interface.** The helper gives
each networked workload a pair of virtual interfaces (§4.1 of `07-networking.md`); the host side's
`rx_bytes` and `tx_bytes` are what the workload sent and received, every packet of it, and survive
a restart of the workload, since the interface is the workload's network rather than an object's.
The split into on-net and off-net is a question of where each packet went, which only the
per-endpoint and per-destination accounting of Phase 2's bandwidth work answers (§7 of
`07-networking.md`); until it is built a sample says its network bytes are not split, rather than
calling them all one or the other. A workload with no network of its own has no network meter.

**What is not measured yet says so.** `device_seconds` waits for devices and `address_hours` for
dedicated addresses, both Phase 3 (§4 of `20-roadmap.md`). A sample names the quantities it
measured, and a quantity it does not name is absent, never zero: a zero is a measurement.

**Storage is sampled, and the samples are stored.** `disk_bytes_stored` is `statfs` of each of the
workload's volumes at the sample, used bytes summed, as the helper reads it for the volume's own
status. `backup_bytes_stored` is measured where backups are stored, not reported by the jobs that
wrote them: once a day the control plane lists each workload's prefix in each repository holding
its backups and sums what it holds, which is the post-deduplication size with the scope named,
since a workload's scope is its own prefix (§5.3 of `06-storage.md`): recipe content it holds by
reference is stored once, outside the prefix, and is metered to no workload. It is the control
plane's own sample, the one metered quantity no node reports.

**The interval is the installation's, and it is an hour by default**, as question 1 of §7 resolved.
`korpis-server serve --metering-interval` declares it, a whole number of minutes from one to sixty
that divides an hour, and each node is told it with its assignment. The node samples each workload
it runs at each interval's end by its own clock, and journals one `MeteringSample` naming the
workload, the interval, the lease epoch it ran under, every cumulative counter and the base under
it, the memory integral, the peak, and what it measured. A sample carries the counters, not the
differences, because a difference is the control plane's to take: so a sample lost does not lose
what it measured, only where within a span it fell.

**The control plane keeps what each interval used, and how it knows.** A record is one workload's
one interval, every quantity a column, and the control plane writes it from two samples: the
quantity is the later counter less the earlier. When the samples between two received ones were
lost, to a journal exhausted or discarded before ingestion (§4.10 of `02-architecture.md`), the
record spans the intervals between them, with the cause: the total is measured, its division
among the intervals is not, and the record says so rather than dividing it. This corrects the
paragraph above that a `journal exhausted` gap "no later delivery can supersede": with cumulative
counters the next sample the node does deliver measures the whole span. What a lost sample does
lose is the division, and the peak, which is per sample.

The node samples every workload it holds an assignment for, running or not, since a stopped
workload still stores its volumes, and a stopped one's CPU is a measured zero. So an interval of an
assigned workload that no record covers once two more intervals have ended is written as a gap
(`node unreachable`), and a record that later covers it supersedes it, as the sections above
require; a node whose journal is merely behind costs a gap and its retraction, both visible. A
machine that restarts loses what its objects used between the meter's last reading and the moment
they ended, since a cgroup does not survive the machine: the agent, starting with a meter whose last
reading is older than the machine's boot, journals that span, at most ten seconds of use and the
time the machine was down, as a gap with the cause `node rebooted`, which no later sample can
supersede. A sample from a node that does not hold the workload's lease at that
lease epoch is recorded as rejected (§4.5 of `02-architecture.md`), which is the fenced cut that
migration needs when Phase 3 builds it.

**Periods are days, and close after a day.** A period is one UTC day of intervals. It is open while
its day runs, closing from its end until the lateness window passes, and closed after that:
`--metering-lateness`, one day by default, a whole number of hours. A record for a closed period is
written into the current open one as an adjustment naming the interval it belongs to (above). A day
is the period because it is the smallest unit invoices are summed from, and closing small periods
often lets a host reconcile as they close rather than once a month.

**Read under `metering.read`.** A new action, read over a workload, a project, or an organization
like every read, and kept while an organization is suspended (§2.1 of `08-identity.md`), since a
suspended tenant is still billed for what it used. A parent holds it over its children by default
(§7 of `08-identity.md`): they are paying for it. The interval a record covers, its source, and its
method are part of the record.

What it costs. Hourly intervals are 24 records a workload a day, a thousand workloads 24,000 a day,
in PostgreSQL; one-minute intervals are sixty times that, the count question 1 of §7 says is stated
when the interval is chosen, and the table is partitioned by month like the effect log. A sample is
a journal record every interval for every workload, which the journal's bound and its reported
horizon now count. The meter reads a cgroup's files every ten seconds per object, which is cheap
but not free on a node of a thousand objects, and replaces the meter's file as often. Memory's
integral is as good as ten-second sampling, stated rather than presented as a counter. A crash of
the machine loses up to ten seconds of each workload's use, recorded as lost. Network bytes are not
split by destination until bandwidth accounting is built.

> Built 2026-10-21 on the node (P2.7b of the build plan, Phase 2 of §4 of `20-roadmap.md`). The
> agent runs every runtime through a metered one, which tells the meter each object it creates or
> recovers and the workload it is, and reads an object's counters just before destroying it. The
> meter reads every object, and the interface `host_link` names for every workload held, every ten
> seconds (or at the interval's own pace where that is shorter), keeps its accounts in `meter`
> beside the journal, and at each interval's end by the node's clock journals a
> `MeteringSample` for every workload held and every one it still reads: the cumulative counters,
> the lease epoch, the interval's peak, the bytes of the declared volumes whose filesystem
> accounts them, and its losses. The build named two causes of loss the design did not: `object
> ended unread`, an object gone before its last counters could be read, whose reading before is
> kept; and `memory unread`, a span of more than thirty seconds between readings, an agent
> stopped, over which memory is not integrated, while the cumulative counters lose nothing. A
> sample's interval is closed only once the journal holds it, so one not journaled leaves its
> peak and losses to the next. The interval is the agent's own option until the control plane
> declares it (P2.7c), and the control plane still records a sample only as having arrived
> (§4.10 of `02-architecture.md`). Unit tests drive the accounts across two objects, a reset, a
> reboot, an unread end, and an agent restart, and an agent test finds a sample an interval
> delivered.

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
audit log that only contains writes cannot answer "did anyone look at this". Browsing a backup and
reading a file from one are recorded as `backup.browse` and `backup.read`, with the path, before
anything is read (§5.4 of `06-storage.md`). A console read is recorded as
`workload.console.read`, by the node that serves it before it sends a record, or by the control
plane for offloaded segments (§8 and §4 of `14-streams.md`); the first was missing until
2026-10-21 (Finding 122 of `23-walkthroughs.md`).

**Tamper-evidence, not just append-only.** `REVOKE UPDATE, DELETE` stops the application; it does
not stop someone with database access. Each day's effects are hash-chained and the day's root is
signed and published. An operator who edits history can still do so, but no longer silently, which
is the achievable guarantee, and claiming more would violate P4.

Export is to open formats for whatever SIEM the operator runs. Tenants can read their own audit
trail scoped by grant, which is unusual in this market and is simply what happens when the audit
log is the same object the authorization model already filters.

**Reading it, as designed.** Designed 2026-10-21, when the tenant-visible audit was built (Phase 2
of §4 of `20-roadmap.md`). Until then `ListEffects` answered one workload, plan, or node, and an
effect named no organization, so an organization could not ask for its own subtree, and an
effect of something done to the organization itself (a grant issued, a theme set, a binding
made) was reachable by no tenant at all. Every effect now names what it is about: `object`, the
object its action was authorized over, or for an effect the control plane writes as its own
actor, the object it acts on (a placement's or a lease's workload, the workload or node of a
node's journal entry); and `organization`, the organization whose subtree holds that object,
resolved by the store from the object's own ancestry in the transaction that writes the effect.
A node's organization is the one it was enrolled into. An effect about nothing (installation,
restore, a relay, an account's own factors and sessions, a caller's own event subscriptions, a
quota's drift corrected) has no organization.

`ListEffects` asks for an organization's subtree or one project, beside a workload, a plan, or a
node. Each effect is shown to a caller who holds, over its object, the read of the effect's family
where the family has one (`grant.read` for `grant.*`, `plan.read`, `extension.read`, `backup.read`,
`event.read`), and otherwise the read of the object's kind (`org.read`, `project.read`,
`workload.read` for a workload and its streams, `volume.read`, `endpoint.read`, `node.read`,
`repository.read`, `domain.read`, `certificate.read`). A denial is shown by the same rule, its
object the one that was asked for, so a tenant sees what was attempted against what it holds. The
fields are filtered as an explanation is (§8 of `08-identity.md`): an effect's node, and a `node`
among its details, are shown only to a caller who may read that node. The actor and the client that
relayed them are shown as they are, an operator's included, since an operator acting on a tenant's
workload is exactly what §7 of `08-identity.md` says the tenant may see. An effect about nothing is
read only under `org.read` on the root organization.

What it costs. Every effect shown is one evaluation against the caller's grants, loaded once a
page. The list pages as every list filtered by read authority does (§8 of `10-api.md`): it reads
past what the caller may not see until its page is full, nothing is left, or it has read eight
pages of effects, and its page token is sealed, so a token names no effect the caller could not
see. A deleted object has no ancestry left to resolve, so an effect of deleting one (a domain, a
certificate) is about the organization that held it. Effects written before this name the
object their workload, plan, or node gives them, and an earlier effect done to an organization
itself names none, so it is read only at the root.

> Built 2026-10-21 in the API (P2.6b of the build plan). `ListEffects` names exactly one of a
> workload, a plan, a node, a project, or an organization, and may ask only for actions beginning
> with a prefix (`grant.`, `workload.restart`). Asking for an organization or a project needs
> `org.read` or `project.read` there, and is otherwise not found, as reading anything is (§8 of
> `10-api.md`); every list then applies the rule above to each effect, the three that existed
> before included, so a caller holding `workload.read` without `plan.read` no longer reads a
> workload's plan approvals. Each effect answers its `object` and `organization`. The page token
> is sealed for the log, its ingest time and identifier inside, where it was the two in clear. An
> API test reads a customer's log as the customer, without `grant.read` and then with it, a page
> of one at a time; finds its own refusal there, recorded as the action it lacked; is not found
> asking for a sibling's; reads both customers' projects as their reseller and by action; and
> has a forged token refused.
>
> Corrected 2026-10-21, after an automated security review of that commit. The rule above was
> built as a filter on two fields and on detail keys `node` and `…_node`, with one `node.read`
> asked over the effect's own node. It let through what named a node any other way: an effect
> about a node read under its family's read (a grant issued over a node, read under
> `grant.read`) kept its object; a lease's `previous_node`, a second node, was never asked
> about; and an enrollment's `node_name` and a key's `node_public_key` matched no key at all,
> so a tenant holding `org.read` read the name of a machine being enrolled into it without
> holding `node.read` anywhere. Now every node an effect names is asked about (its node, its
> journal origin's, its object where that is a node, and each identifier under a node key,
> `node`, `nodes`, `node_…`, or `…_node`, alone or in a list), and a node said without an
> identifier is asked about over what the effect is about, or the root; one the caller may not
> read hides every node of that effect. Unit tests in `korpis-core` cover a lease, an
> enrollment, and a grant about a node; an API test, a placement and an enrollment's name.

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
  key           workload:<id> | organization:<id> | provider:<org>:<type>:<name> | relay:<id>
                | certificate:<id>
                               what it concerns, and so what orders it
  seq           int            per key, from 1, with no gaps: a consumer sees a hole as a hole
  at            Timestamp      the ingest clock, as for an effect
  kind          string         an effect's action, or a condition: `health.changed`,
                               `convergence.changed`, `provider.degraded`, `provider.recovered`,
                               `relay.certificate_expiring`, `certificate.expiring`,
                               `certificate.exposed`, ...
  severity      info | warning | critical
  organization  OrgID
  workload      WorkloadID?
  effect        EffectID?      for an event that reports an effect
  detail        map[str]Value
```

> Corrected 2026-10-14, when relay certificate events were built. The key was listed as a
> workload's or an organization's. A provider's condition has been keyed to the provider since the
> breaker was built (below, under "Degraded dependencies are events"), and a relay certificate's
> approach to expiry is keyed to the relay; both carry the organization they belong to, which is
> what a subscription's reach is judged by.

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
> this database's: a transaction open anywhere on the server, idle or working, in any database,
> holds every event back until it ends. Nothing is lost or reordered, but delivery stalls; §2 of
> `18-operations.md` says what an operator does about it. Narrowing the horizon to this database
> would mean reading other sessions' transaction identifiers from `pg_stat_activity`, which hides
> them from an unprivileged role, and a writer the control plane cannot see would be skipped.

> Corrected 2026-10-14, from a test that failed once in many runs. The argument above holds within
> a batch and not across batches: "a later batch's transaction took the key's row only after the
> earlier one committed" assumes batches follow commit order, and they follow transaction
> identifiers, which are taken at a transaction's first write. A key's second event can come from
> the transaction with the smaller identifier, one that wrote something else first and took the
> key's row only after the first event's transaction committed; with an unrelated transaction
> open whose identifier falls between the two, the second event is below the horizon and the
> first is not, and the second was given the smaller `id`. The sequencer now holds an event back
> while an earlier event of its key still waits, which keeps every key's `id`s in its commit
> order and holds back no other key. The cost is one index on the waiting events by key, and a
> key's later event waiting as long as its earlier one does, which it had to anyway to be
> delivered in order.

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

**Bounded, and the bound is visible.** Delivery reads the log at the subscriber's pace: each
batch is read when the subscriber has taken the last, so a slow subscriber falls behind in the
log, not in a buffer, and holds one batch of memory however far behind it is. The log is kept for
a retention window (thirty days by default, configurable); a subscription whose position falls
out of it receives a gap marker naming where delivery resumes, the oldest event still kept, and
continues from there. Retention never touches effects, which are the audit record and are
archived, never dropped (§4 above).

> Corrected 2026-10-13, when delivery was built. This paragraph gave each subscriber a bounded
> live buffer and ended one that fell behind it with `OVERFLOW`. Delivery that reads the durable
> log needs no live buffer, so there is nothing to overflow and nothing to resubscribe after:
> the one bound left is retention, and it is the gap marker that makes it visible. `OVERFLOW`
> stays for streams with a live buffer (§7 of `10-api.md`). Each batch is judged against the
> subscriber's grants as they are when it is read, and an event it may not read is passed over
> for that stream; it is not acknowledged, so a later connection, holding the grant by then,
> is delivered it.

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

**As built** (2026-10-13). `provider.degraded` is written when a provider's breaker trips, in the
transaction that trips it, keyed to the provider (`provider:<organization>:<type>:<name>`) in the
organization whose installation serves it. It carries the provider, the reason (`failure` or
`latency`), the calls judged and how many failed, the median latency and the median share of the
deadline a call took, and the calls queued behind it. It is a warning, and critical for a
provider whose failure fails closed (identity), since then what it guards is refused to everyone.
`provider.recovered`, at `info`, is written when a probe closes the breaker again, so a consumer
that raised an alert on the first can clear it on the second. Both are read under `event.read`
over that organization; they concern no workload, so a workload selector passes them. The breaker
itself is §5 of `16-extensions.md`.

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
6. **An event's organization.** An event (§5) is keyed to its effect's workload, or else its
   node's organization, or else the organization its grant was scoped over, and that last one is
   where the authority came from, not what was done: an operator who sets a tenant's theme under
   the root grant writes an event to the root's subscribers, not the tenant's. Effects name what
   they are about since the audit was made tenant-visible (§4), so an event could follow its
   effect's object instead, which changes which subscribers hear of what and whether an existing
   subscription goes quiet. → here
