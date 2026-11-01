# Observability

Metrics, the audit log, events, and what Korpis sends home. Metering, the one with financial
consequence, is on [metering](metering.md).

## Contents

- [Metrics](#metrics)
- [Audit is not a separate system](#audit-is-not-a-separate-system)
- [Reading the audit](#reading-the-audit)
- [Events](#events)
- [Ordering](#ordering)
- [Subscriptions](#subscriptions)
- [Conditions](#conditions)
- [Alerting stops at the boundary](#alerting-stops-at-the-boundary)
- [Telemetry](#telemetry)

## Metrics

Prometheus exposition and OTLP; no custom protocol, proprietary agent or bundled dashboard product,
since Korpis is not a monitoring stack. Three scrape surfaces: the control plane's own
(reconciliation lag, plan latency, queue depth, API rates); each agent's (capacity, driver health,
convergence lag per workload); and **a per-tenant endpoint scoped by grant**, where a tenant scrapes
its own workloads into its own Prometheus, seeing exactly what its grants permit and nothing of the
node or its neighbours, at no extra cost since the authorization model already answers what a
subject may see.

**Cardinality is a hard constraint.** Workload identifiers are bounded and are labels; player names,
file paths, remote addresses and anything a tenant controls are not and never become labels, since a
tenant able to inject unbounded label values can take down the operator's monitoring: an
availability vulnerability.

Recent data at full resolution is kept on the node as the workload's `metric` stream, downsampled
there and migrated and offloaded with it, and exported to whatever the operator runs fleet-wide.
Telemetry stays out of the transactional store; metering is the deliberate exception, low in volume
and high in value. Trace propagation across declared dependencies would mean injecting context into
tenant traffic, and is not done.

## Audit is not a separate system

The effect log is the audit log: no second table, no separate writer, so no possibility of the two
disagreeing. An effect is append-only and names the grant that authorized it; an effect of a
control-plane decision is written in the decision's transaction, and one of something on a node is
ingested exactly once, in order, from the node's journal, in the transaction of the state it
explains. Nothing changes state without producing one, because producing one is how state changes.
Bolt-on audit logs are incomplete in exactly the cases that matter, since the paths that skip them
are the unusual ones.

- **Denials are recorded.** What was attempted and refused is the interesting security signal: a
  denied action is an effect naming its subject and the missing action, repeats within a window
  coalesced into one with a count (see [rate limits](rate-limits.md)).
- **Sensitive reads are recorded**, before anything is read: a secret field, a backup browsed or
  read with the path, a console read by the node serving it or by the control plane for offloaded
  segments, an operator attaching to a tenant's console. An audit of writes alone cannot answer "did
  anyone look at this".
- **Tamper-evident, not just append-only.** `REVOKE UPDATE, DELETE` stops the application, not
  someone with database access. Each day's effects are hash-chained and the day's root signed and
  published, so an operator who edits history can do so, but not silently: the achievable guarantee,
  and claiming more would be false.

Export is to open formats for whatever SIEM the operator runs. Effects are archived, never dropped.

## Reading the audit

Every effect names what it is about: `object`, the object its action was authorized over, or for an
effect the control plane writes as its own actor, the object it acts on; and `organization`, whose
subtree holds that object, resolved from the object's ancestry in the transaction writing the
effect. A node's organization is the one it was enrolled into. An effect about nothing
(installation, restore, a relay, an account's own factors and sessions) has no organization and is
read only under `org.read` on the root. A deleted object has no ancestry left, so an effect of
deleting one is about the organization that held it.

`ListEffects` names exactly one of a workload, plan, node, project or organization's subtree, and
may filter by action prefix (`grant.`, `workload.restart`); asking for a project or organization
needs `project.read` or `org.read` there, and is otherwise not found. Each effect is shown to a
caller holding over its object the read of its family where it has one (`grant.read` for `grant.*`,
`plan.read`, `extension.read`, `backup.read`, `event.read`), else the read of the object's kind. A
denial is shown by the same rule, so a tenant sees what was attempted against what it holds. **Every
node an effect names** (its node, its journal origin's, its object where a node, each identifier
under a node key, `node`, `nodes`, `node_...` or `..._node`, alone or listed, and a node named
without an identifier) is asked about, and one the caller may not read hides every node of that
effect; a filter on a few field names let a tenant read the name of a machine being enrolled into it
without `node.read`. The actor and the relaying client are shown as they are, an operator's
included, since an operator acting on a tenant's workload is exactly what the tenant may see. The
list pages as every authority-filtered list does, with sealed tokens; each effect shown is one
evaluation against the caller's grants, loaded once a page.

## Events

```
Event
  id            global, increasing; a consumer's position is the last it acknowledged
  key           workload:<id> | organization:<id> | provider:<org>:<type>:<name> | relay:<id>
                | certificate:<id>        what it concerns, and so what orders it
  seq           per key, from 1, with no gaps: a consumer sees a hole as a hole
  at            the ingest clock
  kind          an effect's action, or a condition: health.changed, convergence.changed,
                provider.degraded, provider.recovered, certificate.expiring, ...
  severity      info | warning | critical
  organization, workload, effect, detail
```

**Every event is written in the transaction of the fact it reports.** Every effect is an event of
its own action, so the log of what happened and the stream of what to react to cannot disagree; a
condition is written where the control plane decides it, never afterwards by a process that might
not run. An effect's event carries its `after` and `error`, `info` for success or skip and `warning`
otherwise; `critical` is for conditions. It is keyed to the effect's workload, else the organization
owning its node or its grant's scope. An effect concerning no organization (bootstrap before any
exists) is no event, since every event is read under `event.read` on an object. Kinds are dotted
words, so `provider.` selects every provider event.

## Ordering

Writing an event takes its key's counter row, so a key's events are numbered in commit order. An
event is inserted with an internal insertion number, and a **sequencer** gives it its `id`, a dense
position, only once its transaction and every older one have ended, below PostgreSQL's snapshot
horizon, in batches ordered by insertion number. A key's later event could come from a transaction
with a smaller identifier that took the key's row only after the earlier event's transaction
committed, so the sequencer also holds an event back while an earlier event of its key still waits,
keeping every key's ids in commit order and holding back no other key. So delivering by `id` never
hands out a key's events out of order or passes one over. The cost is one sequencer pass of delay
(woken by every event's commit, and once a second), one sequencer at a time, which control-plane
processes take turns being, and one counter row update per event, serializing one workload's events.

**The horizon is the whole PostgreSQL cluster's**: a transaction open anywhere on the server, in any
database, holds every event back until it ends. Nothing is lost or reordered, but delivery stalls
(see [availability](availability.md#postgresql)). Narrowing it to one database would mean reading
other sessions' transaction ids from `pg_stat_activity`, which an unprivileged role cannot see, and
a writer the control plane cannot see would be skipped.

## Subscriptions

**A subscription is a stored position, not a connection**: its subject, a filter (kinds, a minimum
severity, a selector over projects and workload labels), and the last `id` acknowledged. Delivery is
at-least-once, ordered per key: a reconnecting consumer resumes after its last acknowledgement and
may see again what it did not acknowledge, the `id` making it idempotent. Only events about objects
the subject holds `event.read` on are delivered, judged by its grants as they are at each send; an
event it may not read is passed over for that stream and not acknowledged, so a later connection
holding the grant gets it.

A subscription belongs to the subject that made it, and to anyone else it does not exist. Making one
needs no grant, since it holds nothing; making and deleting are effects, acknowledging is not. A new
subscription starts at the end of the log, and an acknowledgement may not pass the newest positioned
event. An extension's subscriptions are made with its installation, held by `extension:<id>`,
narrowed to its target project, and deleted on uninstall.

**Bounded, and the bound is visible.** Delivery reads the durable log at the subscriber's pace, a
batch when the last was taken, so a slow subscriber falls behind in the log and holds one batch of
memory, with no live buffer to overflow. The log is kept thirty days by default; a position falling
out of it receives a gap marker naming where delivery resumes. Deriving the stream from the effect
log afterwards could not promise per-workload order across several writing processes, which is why
an event row is written beside every effect, about doubling the audit path's writes.

## Conditions

**`health.changed`** is written in the transaction recording the observation it differs in.
**`convergence.changed`** has no such transaction, since convergence is derived when read (target,
plans, observation, clock) and an observation going stale changes it with no write; the control
plane decides it on every workload change and every resync, records the decision, and writes the
event when it differs from the last, swapping the record so one of several processes writes it. A
change only the clock makes is reported up to one resync late. Both carry `from` and `to`; a change
for the worse is a warning.

**Degraded dependencies are events.** A failing provider trips its breaker, but a slow one produces
no error anywhere while consuming the fleet's reconciliation capacity, so the operator whose creates
got slow would have to infer the cause from a graph. `provider.degraded` is written in the
transaction tripping a breaker, keyed to the provider, carrying the reason (`failure` or `latency`),
the calls judged and failed, the median latency and share of the deadline, and the calls queued; a
warning, critical for a provider whose failure fails closed. `provider.recovered` is written when a
probe closes the breaker, so an alert raised on one is cleared on the other. See
[extensions](extensions.md).

## Alerting stops at the boundary

Korpis emits events with severity, subject and detail, and builds no routing, deduplication,
escalation, on-call schedules or silences, which Alertmanager and PagerDuty do better. What ships is
a selector, a severity and a destination (`project "community-servers", severity >= warning`,
delivered to a chat channel), the destination holding a grant and receiving only what it permits.
Anything richer is an extension or an external system consuming the published stream. It is health,
not process liveness, that generates events ([health](health.md)).

## Telemetry

Usage telemetry from an open-source project is a trust decision, so it is off by default and opt-in,
and the exact payload is printable first (`korpis telemetry show`, then `enable`). No instance
identifier is sent unless enabled, nothing about tenants, workloads or names is ever included, and
the aggregate is published to the people who supplied it. Quietly measuring growth would be another
way of taxing trust.
