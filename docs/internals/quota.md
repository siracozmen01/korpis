# Quota

How a tenancy tree bounds what each level may consume, and how a reseller oversells honestly. What a
workload reserves and what the kernel enforces is on [resources](resources.md).

## Contents

- [Quota sets](#quota-sets)
- [Reservations sum, limits may oversell](#reservations-sum-limits-may-oversell)
- [Admission holds what it checks](#admission-holds-what-it-checks)
- [Setting a quota set](#setting-a-quota-set)
- [Where a workload runs](#where-a-workload-runs)
- [Quota holds the enforced value](#quota-holds-the-enforced-value)
- [The egress budget](#the-egress-budget)

## Quota sets

A host sells "4 GB in the EU", and capacity in one region is not interchangeable with another, so
one global bound cannot express what is sold.

```
QuotaSet
  entries []QuotaEntry
QuotaEntry
  selector       workload labels it counts; none counts everything
  nodes          node labels it counts by; none counts from declaration, wherever it runs
  reservations   guaranteed; sums against the parent
  limits         ceiling; may exceed the parent by a declared ratio
  version        part of the admission compare-and-swap
```

Admission requires an intent to fit every matching entry, so a tenant holds a global 32 GB cap and a
4 GB per-region cap at once. The dimensions are CPU, memory, disk, I/O ceilings, monthly egress,
bandwidth, counts (workloads, volumes, endpoints), dedicated IPv4 addresses, node ports, and devices
by class. Dedicated IPv4 and node ports are exclusive and never overcommitted. IPv6 addresses are
not a dimension: they are abundant, and counting them would make scarce and plentiful look alike.

**Every dimension is enforced by the component that can enforce it, and metered.** Per-workload
resources are enforced by the kernel at the node. Aggregates (an organization's memory across fifty
nodes) and counts (how many workloads) no single kernel can enforce, so they are enforced at
admission, in the transaction that would exceed them. A dimension nothing can enforce is not added.
The name is quota, not plan: plan is taken, and a commercial plan is the billing system's concept.

## Reservations sum, limits may oversell

A reseller holding 32 Gi sells 16 Gi to each of two customers and wants to sell a third, because
customers use a fraction of what they buy; overselling is the ordinary practice of every host Korpis
courts. Counting a child's quota by allocation forbids it; counting by usage makes quota no
guarantee, breaking the first time three customers get busy. So quota draws the distinction a
workload's resources already do:

| | Against the parent | Meaning |
|---|---|---|
| reservations | sum, never oversubscribed | what the reseller has guaranteed |
| limits | may exceed, by a declared ratio per dimension | what the reseller has sold |

A reseller sells 16 Gi limits backed by 4 Gi reservations and sees both totals, promised and
guaranteed: the spreadsheet they keep today, made a first-class number. Memory overcommit is off by
default, so a parent that has not enabled it cannot oversell memory.

**A guarantee is held where it is evaluated.** An organization's entry counts each child's entry of
its shape as the larger of what the child was given and what it holds, beside what the
organization's own workloads hold. A workload within its child organization's guarantee is not
refused by the entry above that child, whatever the parent itself runs. So promising is reserving: a
reseller's own servers run only in what it has not promised. The control plane evaluates this at
admission and at a set's change, from the entries it locks and those of the organizations directly
beneath, read under the parent entry's lock.

## Admission holds what it checks

Protecting a declaration with a compare-and-swap on the workload protects the wrong object: quota
belongs to the organization, so two concurrent creates each see 24 Gi of 32 used and both commit.
**Quota consumption is written in the same transaction as the intent, against the matching entries,
whose versions take part in the compare-and-swap.** The loser gets `CONFLICT` and re-admits. A quota
checked without being held is advisory, and an advisory limit is not displayed as a limit.

- **Which entries:** the workload's project's, and its organization's and every ancestor's, so a
  reseller's 32 Gi bounds its customers' customers. Evaluated at declaration from the tenancy tree.
- **What a selector matches:** the workload's labels, the only description admission has.
- **What a workload holds:** per dimension, the larger of its declared head and its approved target,
  since either may be what runs soon. An unset limit is the reservation; an unset I/O ceiling is
  unbounded, which no bounded entry admits. Disk includes every volume the workload creates and the
  most its console stream can keep, which placement reserves on the node too.
- **Only growth is refused.** A declaration holding no more than before in a dimension passes even
  where the entry is exceeded, so lowering a bound never strands a workload that then cannot be
  edited, even to shrink.
- **A refusal names the set only to whoever may read it**: the owner, the bound and what would be
  held, to a caller with `org.read` or `project.read` there; anyone else, such as a customer refused
  by its reseller's set, learns only the dimension and side. Both are `QUOTA_EXCEEDED`.
- **Holdings are kept per entry and per workload**, never as a free-running counter. Each entry
  keeps totals of what its workloads hold, and each workload keeps what it was last counted as
  holding and by which entries. Every change to what a workload holds (a declaration, an approval, a
  cancellation, a recovery, a retirement) recounts that one workload in its own transaction, so a
  declaration reads two intents and one row per entry whatever the entry counts. Entries are locked
  in identifier order, after the workload's row and before the placement lock, so nothing deadlocks.
  A reconciliation at start and hourly recounts every workload from its intents and reports any
  correction as `quota.drift`, since only a change that skipped its recount makes one. A dry run
  admits and rolls back, so `korpis plan` shows a quota refusal before anything is sent.

## Setting a quota set

An organization's set is its parent's decision: `SetQuota` replaces it under `org.quota.write` over
the parent, so no tenant raises its own. A root organization holds no set, since what it holds is
the installation's and no host is limited. A project's set is a sub-limit within its organization's,
under `org.quota.write` on the organization.

The change is checked in one transaction holding the locks of every set it is checked against.
Entries are compared by shape (their workload and node selectors), because entries of one shape at
two levels are the same promise made at each. Per shape and dimension: the reservations given to the
organization and its siblings, with what the parent's own workloads hold, sum within the parent's
reservation; their limits sum within the parent's limit times the overcommit percent the parent was
permitted (none for dedicated IPv4 or node ports); what the organization gives its own children
still fits, so lowering a reseller below what it gave is refused (`quota.overallocated`) until the
children are lowered; and each project's entry fits its organization's. A shape the parent has no
entry of holds the children to nothing at that level, though admission still counts them against
every entry above. A bound of zero is a bound (the wire carries bounds with explicit presence).
Lowering a bound below what is held is allowed, since admission refuses only growth.

A change first locks the organization rows above it and its own, top down, before reading any set,
so two siblings cannot each be given the last of their parent's reservations: locking only existing
entries would miss entries another change had just made, under read-committed isolation. Workloads
made in the organization meanwhile do not wait, since the lock leaves the row's key alone. An entry
whose shape a new set keeps keeps its identity and totals.

## Where a workload runs

"4 GB in the EU" is capacity in a place, decided by placement after admission. So an entry may name
the nodes it counts by node labels. It counts a workload its selector matches while that workload is
placed on a matching node, at what the placement runs there. It is a filter check of placement: a
node is refused while what the workload would run there exceeds such an entry, with the entry,
dimension and figures as the reason, so a workload that may run anywhere goes where there is room
and one that must run in the EU waits, unplaced, saying why. Manual placement is refused by it too,
since it is the tenancy's quota, not the operator's decision about a node.

Every change to a placement (bind, follow in place, unbind, a lost node, recovery) recounts where
that one workload is counted, under the placement lock that every such change holds; these entries
are never locked at declaration. Node labels are set at enrollment and never change, so a node never
moves between regions under a placement. A workload admitted within its global entries may still
find no node with room in its region: it is unplaced with the reason, since a declaration cannot
know where it will run.

## Quota holds the enforced value

A declared set may drift on purpose: someone raises a memory limit through the panel at 03:00 to
stop an OOM loop, the set is marked drifted, and the change stands. For as long as it lasts, the
file says 4 Gi and the cgroup says 8 Gi. Quota is held against what the kernel actually grants,
because capacity is what is sold and a reseller's headroom is consumed by the enforced number;
holding the declared value would let a tenant exceed a guarantee by drifting. A drifted set reports
its quota delta beside the drift in `korpis status`, so the fix that consumed a reseller's headroom
is visible the same morning.

## The egress budget

A budget bounds what was used, so it is enforced after use, by shaping. An entry's limits may carry
`egress_bytes_per_month` with `egress_exhausted`, either a floor rate or a cut-off; the two are set
together, a floor of zero is refused as an unnamed cut-off, and a budget that changes nothing when
spent is refused, since it would be a number displayed and not enforced. The month is a calendar
month of UTC days, summed as an invoice sums them; a host billing from a customer's signup day keeps
that budget itself.

Every minute, with metering's pass, the control plane sums for each entry with a budget the egress
its workloads' records hold this month (its owner's workloads its selector matches, and for an entry
naming nodes, sent from those nodes), kept apart from the entry so reckoning never waits on
admission; `korpis quota show` says what was spent and when reckoned. An entry counts a workload it
selected once in a month for the rest of that month, whatever the workload later says of itself, so
relabeling cannot lift a hold. Each workload a spent entry counts is held to the strictest of its
spent entries (a cut-off before any floor, the lowest floor before a higher) in a row whose trigger
announces the workload, so its next assignment carries the hold; the month's turn lifts every hold
at once, and a raised budget lets go at the next pass. A floor holds the workload's link and its
uplink class to that rate; a cut-off lets nothing leave its link for beyond the node, replies
included, since a served workload's egress is almost all replies. The agent makes a running
workload's network again whenever what it would ask the helper changes, so a hold is in the kernel
within one pass. A node that cannot shape holds a floor as a cut-off, the stricter, rather than hold
nothing, and logs why.

**What it costs.** A budget is overrun by what its workloads send before it is known spent: up to
one metering interval (an hour by default) at their ceilings, and a node that cannot reach the
control plane keeps its last state. The bytes counted are those that left the workload's link for
anything but another workload on its node, so traffic to the node's own ingress proxy counts.
