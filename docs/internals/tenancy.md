# Tenancy

How organizations and projects nest, and how a host suspends a customer without destroying what
the customer built. Quota sets are on [quota](quota.md); authority is on [grants](grants.md).

## Contents

- [The tree](#the-tree)
- [Suspension](#suspension)

## The tree

```
Organization -> Organization -> Organization -> ...
     |               |
     +-> Project     +-> Project -> Workload
```

Organizations nest without limit. Each holds a quota set whose reservations never exceed its
parent's unallocated reservations, and whose limits may exceed the parent's only by the overcommit
ratio the parent enables. Projects group workloads inside an organization and are the usual scope
of a grant: a community's game network and its website are two projects, and a moderator can hold
authority over one and not the other. Projects, workloads and nodes carry labels in the syntax of
Kubernetes labels (an optional DNS-subdomain prefix and `/`, then a name of at most 63 characters
of `[A-Za-z0-9-_.]` that begins and ends alphanumeric; a value is empty or such a name), because
operators already know it and a selector reads the same in both systems. The API refuses anything
else.

There is no reseller object, and there never will be:

| Real role | In Korpis |
|---|---|
| Hosting provider | a root organization |
| Their customer | a child organization with a quota set |
| That customer's customers | grandchildren |
| A community's admin team | a grant scoped to a project |
| One game server's owner | a grant scoped to a workload |

Nothing in the code distinguishes these levels. TCAdmin sells reseller support as a premium
feature because its tenancy was not built for it; here it is what correct tenancy produces.

Identifiers are immutable and never reused; names are mutable and may be. Deletion is soft at the
identity layer, so effects, streams, backups and metering are never attributed to a different
object. The tree has no cycles, and an organization is deleted only once its subtree is empty.

## Suspension

The most common thing a host does to a customer is suspend them for non-payment and restore them
a week later. Revocation would be the wrong lever: it is irreversible and cascades, so a customer
suspended that way would lose every moderator grant, share link and CI token, and rebuild them by
hand on return. Suspension is reversible.

- `org.suspend` and `org.resume` are separate actions, so a billing system can be let restore a
  customer without being let suspend one. Both are authorized over the organization's parent: a
  grant scoped at the organization itself never covers its parent, so a tenant cannot resume
  itself, and a root organization is suspended by no one.
- A suspended organization's grants, and those of every organization beneath it, stop authorizing
  and are not destroyed; resuming restores them unchanged. The cascade is derived: the ancestry
  of an organization carries whether each organization in it is suspended, and a grant scoped
  within a suspended subtree authorizes only the reads a suspended tenant keeps. Suspending a
  reseller with forty customers writes one row.
- A suspended tenant keeps `grant.read`, `org.read`, `project.read`, `workload.read`,
  `plan.read`, `node.read` and `metering.read`, because a customer asked to pay must see what for.
  Reaching the service is what is withheld: consoles, files, a backup's contents, and every change.
  A grant scoped above the suspended organization acts on it as before. A workload holding a grant
  scoped in it loses its API relay with its next assignment.
- The suspension says what happens to workloads: `keep_running` (access suspended, service
  continues) or `stop`. Volumes and backups are untouched either way; deleting a suspended
  customer's data is a separate, irreversible plan made on the record.

**`stop` is derived, not written onto workloads.** While an organization is suspended with `stop`,
every workload beneath it, in nested organizations too, is told stopped by its assignment whatever
power it declares. Nothing is written onto any workload, so the tenant's declared power stands, a
workload declared or started meanwhile is held like the rest, and resuming returns each to its own
power. A second suspension, above or below, holds what it covers until it is itself resumed. A
plan waiting to apply keeps waiting unless it retires the workload; one already applying finishes,
since a node left halfway through its steps would hold what no plan made. The workload's status
says it is held, and its convergence is measured against the stopped power its node is told, so it
reads as converged rather than failing. Writing a stop per workload was tried and cost three
defects: workloads declared while the list was taken escaped, resuming an ancestor started what a
suspended descendant still held, and a power change mid-stop was skipped silently.

A stop changes what the tenant's workloads do, so suspending with it also needs `workload.stop`
over the organization, and resuming such a suspension needs `workload.start`: a billing system let
suspend is not thereby let take servers down. Both are decided against the organization as read,
and the status is written only if it is still at that version.

**What it costs.** The hold is not on each workload's record: a workload's history shows its own
declarations, and the organization's `org.suspend` effect, naming the choice, is the record. Every
assignment, run pass and status read asks the workload's ancestry, one more query each.

Billing systems need exactly this lever from Korpis: Korpis provides the reversible state, and
deciding when to apply it is the billing system's job.
