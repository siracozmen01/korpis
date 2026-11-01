# Workloads

The workload, its declarations and what is observed of it. How a declaration becomes a change is on
[plans](plans.md); how a node is told is on [reconciliation](reconciliation.md).

## Contents

- [The workload](#the-workload)
- [Intent](#intent)
- [Power](#power)
- [Declared sets](#declared-sets)
- [Observation](#observation)

## The workload

A workload is the stable identity of a thing that runs, deliberately almost empty, to which state
attaches.

```
Workload
  id              immutable, never reused
  project, organization   the organization denormalized so authorization reads one row
  name, labels    mutable; the name unique within the project
  intent          the newest declared intent: the head
  target          the approved intent the system converges toward
  power           the newest power declaration; power_target the approved one
  observation     the latest observed state
  set             the declared set it belongs to
  classification  the operator's overrides of step classification, at most one per action
  version         its metadata's version, compared by changes to it
  unreproducible  set once, with a reason, when what it runs stopped following from its
                  recipe and lockfile alone; never cleared
  created, deleted_at
```

An id is never reused, even after deletion: logs, backups, grants and metering outlive the workload
and must never be attributed to another. Nothing references a workload by name.

**The head and the target are different pointers, and only the target reaches a node.** Declaring
moves the head; approving its plan moves the target. While they differ the workload is awaiting
approval or scheduled, and nothing on any node has changed. With one pointer, a node would converge
toward a change nobody approved, bypassing approval, and "approve now, apply in a window" would have
nowhere to wait. Power has the same two pointers for the same reason: a stop that waits for
confirmation must not reach the node when declared. A workload's first approval approves its first
power declaration with it, since that plan's steps were computed with it.

**Unreproducible** marks a workload whose install took a step provider's unpinned answer, or that
was declared from an unverified import: its volumes hold bytes its recipe and lockfile do not
determine, and no later install undoes that. It is a label, not a gate, saying which workloads the
recipe's "one digest, the same bytes" promise does not cover. The first reason stands.

## Intent

What someone declared should be true: immutable, versioned, chained to its parent, recording who
declared it and, when declared through a declared set, which.

```
Intent
  recipe        name, version, digest
  lifecycle     persistent | task | scheduled; schedule and timezone for scheduled
  runs          run policy, for task and scheduled
  console       none | logs | interactive
  runtime       driver, isolation tier, driver config
  resources     cpu, memory, disk, io, bandwidth, pids
  devices       class, count, selector, mode
  volumes, endpoints, health, dependencies
  config        validated against the recipe's schema
  placement     constraints, affinities, preferred region
  continuity    on_lease_expiry (derived unless set); replica_failover (manual unless set)
  streams       the console stream's rate, burst, retention, offload retention
  egress        where it may connect out beyond the default, and its own lower limits
```

An intent is never mutated: any change is a new intent with the next version. `config` is validated
against the recipe's schema at declaration, so an invalid intent is refused before it exists, not
discovered at apply time. There is no anonymous change. Because intents are an immutable chain, a
workload's declared history is a linked list, and rollback is declaring intent N again rather than
an inverse operation. Outbound rules belong to the workload, not to an endpoint, since a connection
a workload opens leaves from no endpoint; the endpoint field stays in the frozen schema and a
declaration setting it is refused, naming where the rules now go.

## Power

Whether a workload should run is declared apart from its configuration:

```
Power
  desired    running | stopped
  cycle      incremented to request one restart; performed once per value
  version    its own monotonic sequence
  set_by, set_at
```

Start and stop are the most frequent operations there are. Inside the intent body, each would copy
the whole body into a new immutable row, the intent chain would become a power log, and "roll back
to intent 41" would mean nothing when 41 and 42 differ only in being stopped. Power is versioned,
attributed and reconciled like everything else, so a stop still produces a plan and an effect naming
its grant; it just produces no intent. Re-declaring intent 41 restores a configuration and says
nothing about whether the workload should run, which is what a rollback should mean. A restart is a
higher cycle, which the agent performs once.

## Declared sets

A declared set is the group of workloads one declarative source owns: the files a pipeline applies
with `korpis apply -f servers/ --set community-servers`.

```
DeclaredSet
  project     a set and its members live in one project
  name        unique within the project
  mode        advisory | strict: what declaring through it does with drift
  version, created_by, created_at
```

- **A workload belongs to at most one set, and a set touches only its own.** A workload joins by
  being created through the set, or by a declaration through it that asks to adopt a workload in no
  set. A declaration through a set onto another set's member, or onto an unadopted workload, is
  refused, so a hand-made workload is never claimed, and never pruned, by someone's pipeline.
- **Drift is derived, never stored.** A member has drifted when its head was not declared through
  its set (someone used the panel, a chat command or an imperative command). The head already names
  who and when, so stored drift would be a second copy that could disagree.
- **Declaring through a set onto a drifted member**: a body equal to the head is the files catching
  up with the fix, accepted in either mode (`set.drift_adopted`); otherwise a strict set declares
  over it, and an advisory set refuses with `CONFLICT`, reason `set.drifted`, unless asked to
  reconcile. A discarded intent and its author are recorded (`set.drift_discarded`). Advisory is the
  default, because punishing people for fixing an outage at 03:00 is how a tool gets worked around.
- A set is created by the first declaration through it, in advisory mode. Changing its mode needs
  `project.write`; reading sets, `project.read`.

A set spanning projects is not expressible: a pipeline applying to three projects holds three sets
of the same name, because the project is where workload authority and names live.

## Observation

```
Observation
  workload, node, observed_at
  assignment_seen   the assignment generation applied
  intent_seen       the target intent converged to
  power_seen        the power version and cycle performed
  plan_progress     the last plan step completed
  state             running | stopped | starting | stopping | crashed | unsatisfiable | unknown
  reason            for unsatisfiable: the structured reason
  health            healthy | unhealthy | unknown
  exit_code, runtime_id, resources, endpoints, volumes
  since             when the state last changed, as the node recorded it
```

Observations are written only by the agent and intents only by the control plane: separate objects,
writers and trust levels. Merging them into one spec and status record invites exactly the "the
panel says stopped but it is running" class of bug. An unreachable agent's workloads are unknown,
never a remembered value presented as current. An observation reports only what the node can know:
whether a plan failed, was cancelled or awaits approval is a fact about the plan, held by the
control plane, and never an observed state.

A node that cannot satisfy its assignment does not reject it, since rejection is a decision and
decisions are the control plane's. It reports a fact: `unsatisfiable`, with a reason
(`missing_driver`, `insufficient_resources`, `volume_unavailable`, `device_unavailable`,
`image_unsupported`, `prepare_rejected`) and the driver's detail. The control plane decides from the
journaled step failure, which is ingested exactly once and says whether anything was made on the
node, whether to place elsewhere, surface it or wait.
