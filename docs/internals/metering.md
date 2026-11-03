# Metering

What a workload used, measured from kernel counters, exact, gap-marked and idempotent, for billing
systems to invoice from. Korpis does not bill; it hands a billing system numbers it can stand
behind.

## Contents

- [Four systems, not one](#four-systems-not-one)
- [Measured, never assumed](#measured-never-assumed)
- [What is metered](#what-is-metered)
- [On the node](#on-the-node)
- [In the control plane](#in-the-control-plane)
- [Gaps](#gaps)
- [Periods](#periods)
- [Continuity across migration](#continuity-across-migration)
- [Reading it](#reading-it)
- [What it costs](#what-it-costs)

## Four systems, not one

Panels in this market build one subsystem called "stats" and use it for everything, which is why
their billing numbers are wrong: a series made to draw a graph is sampled, lossy and interpolated,
correct for a graph and unacceptable for an invoice.

| | Consumer | Guarantee | Retention | Store |
|---|---|---|---|---|
| metrics | operators, dashboards | best effort, lossy | short at full resolution | time series, outside PostgreSQL |
| metering | billing systems | exact, gap-marked, idempotent | years | PostgreSQL, partitioned |
| audit | operators, tenants, compliance | append-only, attributable, tamper-evident | years | the effect log |
| alerting | people | delivered or visibly not | none | none |

Collapsing them could not be undone later, because by then invoices depend on it. Metrics, audit and
events are on [observability](observability.md).

## Measured, never assumed

Every metered quantity comes from a kernel counter (cgroup v2, the filesystem's quota accounting,
nftables counters) or from what a placement holds for the time it holds it. **No metered quantity is
derived from a configured limit.** Pterodactyl reports the limit set as though it were consumption,
because it cannot measure the difference, and here the number becomes an invoice. A quantity not
measured is absent, never zero: a zero is a measurement.

## What is metered

| Quantity | Read from |
|---|---|
| `cpu_seconds`, throttled time | `usage_usec` and `throttled_usec` of `cpu.stat` |
| `memory_byte_seconds`, peak | `memory.current` integrated every ten seconds; `memory.peak` |
| `disk_io_bytes`, read and written | `rbytes`, `wbytes` of `io.stat`, summed over devices |
| `disk_bytes_stored` | each volume's quota accounting at the sample, summed |
| `network_bytes`, in and out | the host side of the workload's link, `rx_bytes`, `tx_bytes` |
| egress, and bytes by endpoint | the host table's named counters ([bandwidth](bandwidth.md)) |
| `dedicated_core_seconds`, `device_seconds` | each held core or device for every second its placement holds it |
| `backup_bytes_stored` | the control plane, listing each workload's prefix daily |
| `address_hours` | not measured yet |

Memory is metered as byte-seconds because that is what capacity costs, and the peak beside it
because that is what people argue about; which a billing system charges on is not Korpis's decision.
A held core or device is the workload's from bind to freeing, used or not, which is what makes it
its, so it is integrated by the agent from the assignment, and nothing measures how busy a device is
or charges by it. A guest's disk is its images' allocated blocks, labelled `disk_allocated` wherever
shown. Backup bytes are post-deduplication with the scope named, the workload's own prefix; recipe
content held by reference is metered to no workload.

## On the node

**Every counter is cumulative per workload.** A workload outlives its objects: a restart, a run and
an upgrade each make a cgroup whose counters begin at zero. So the agent's meter keeps, per
workload, a base that each source (an object, or the network interface) adds its final reading to
when it ends, the workload's counter being the base plus its current sources' readings. An object's
final reading is taken before the agent removes it, since a cgroup's counters are readable until
then. A reading lower than the last is a source remade unseen: the last reading joins the base and
what it used since is a gap, `counter reset`. The meter keeps its accounts (`meter`, beside the
journal) in a file replaced atomically at every reading, so a restarted agent takes up where it
stopped; a restarted driver loses nothing, the cgroups being the kernel's.

**Memory is integrated on the node, and says so.** The kernel keeps no integral, so the meter adds
`memory.current` times the time since the last reading every ten seconds; its error is how much
memory moved within ten seconds, and the method is published with the number. An interval's peak is
the most read within it, and where an object's `memory.peak` rose since the reading before, that
mark, so a spike that set a new high between readings is caught. A span over thirty seconds between
readings (an agent stopped) is not integrated, `memory unread`; an object gone before its last
reading keeps the one before, `object ended unread`.

**The interval is the installation's**, an hour by default, one to sixty whole minutes dividing an
hour (`korpis-server serve --metering-interval`), told to each node in the `Welcome` of its session.
Totals lose nothing at any interval, since the counters are cumulative; what a coarse interval loses
is shape, which is why each record carries the interval's peak memory and throttled time, the number
a dispute about dedicated capacity turns on. At each interval's end by its own clock, the node
journals one `MeteringSample` per workload it holds an assignment for, running or not, since a
stopped workload still stores volumes and its CPU is a measured zero: the cumulative counters, the
lease epoch, the peak, what it measured and its losses. A sample carries counters, not differences,
so a lost sample loses only where within a span its use fell. An interval is closed only once the
journal holds its sample.

The meter's journal is the node's, shared with effects, so there is one bound, one horizon in
`NodeStatus`, and one place an operator raises it: the trade between local disk and how long a node
may be partitioned before its meter has holes.

## In the control plane

**A record is one workload's one interval**, every quantity a column, written from two samples as
the later counter less the earlier, measured from that workload's last sample on that node
(`metering_reading`). Where samples between two received ones were lost, the record spans their
intervals with the cause `samples lost`: the total is measured, its division among the intervals is
not, and the record says so rather than dividing. The first sample on a node counts from zero; a
lower count begins again with `counter reset`; a sample ending no later than the last is rejected. A
sample is believed from a node that held the workload's lease or is placed with it, and checked
first: an interval of at most 31 days, times within 400 days before and an hour after the control
plane's clock, every counter fitting the store, no epoch past the lease held; one that fails is
`journal.rejected` and the journal moves on, so one bad sample cannot stop ingestion. Records are
appended to `metering_record`, partitioned by month of recording.

**Idempotent by construction.** A record's identity is `(workload, node, interval_start)`, and
re-delivery of a journal record is dropped by the journal cursor, so an agent re-sending an hour of
buffered records after a reconnect, a normal event, cannot double-bill. **Changing a number appends;
it never overwrites**: a sample replacing a gap, a late sample and a correction insert a new version
with a `supersedes` list, and the current value is the newest version, so what an interval said on
the day it was invoiced is a query. A record's endpoints are one document of the record, since a
record is never changed and always read whole.

## Gaps

A missing interval is a gap with its cause, and the billing system sees the hole; interpolating
would bill a customer for a period nobody measured. But a gap in delivery is not a gap in
measurement: a node that lost its uplink kept its cgroups. So a gap is a claim about the control
plane's knowledge, sharing a record's identity, and a real sample for its interval supersedes it,
the retraction dated and visible, since a number that changed is one a host may have to explain.

Every minute the control plane writes a gap, `node unreachable`, one record per interval of a
workload on its placed node that nothing covers once two more intervals have ended, from its last
sample or gap there, or from its placement where the node never sampled it, so a node that does not
meter leaves gaps rather than silence; at most a week of them a pass. A machine that restarts loses
what its objects used between the meter's last reading and their end, since a cgroup does not
survive it: the agent, finding its last reading older than the boot, journals that span, at most ten
seconds of use and the downtime, as `node rebooted`, which nothing later supersedes. A sample from a
node not holding the lease at its epoch is rejected.

## Periods

A billing system needs finality, not a moving target. A period is one UTC day of intervals: open
while it runs, closing until the lateness window passes (`--metering-lateness`, whole hours, a day
by default), then closed. A record whose interval begins in a closed day is written into the current
one as an adjustment, dated by when it was recorded and naming its interval, never a silent edit, so
a host that invoiced can reconcile and one that has not can wait for `closed`. The window is stated,
not convenient: long tolerates partitioned nodes and delays every invoice; short bills on time and
sends more adjustments. A day is the period because it is the smallest unit invoices are summed
from. A correction path for a metering bug found after invoicing (append-only records with a
`supersedes` link, through an API) is not designed yet.

## Continuity across migration

Counters are per workload, not per process, container or node: a restart does not reset its meter,
and a migration neither double-counts nor drops, because migration has exactly one fenced instant at
which ownership moves. The source journals a last sample when it releases its lease at `cutover`;
the destination meters nothing of a workload it holds as a destination until granted the next epoch,
its first sample then counting from zero; a sample of either node under an epoch it does not hold is
rejected; and gaps are written for the authoritative placement alone, so neither the destination's
wait nor the source's hold is a gap. The interval the cutover falls in is the one both nodes
measure, each its own share under its own epoch, which is why a record's identity names its node.
What the source keeps through the hold is not metered: it is the installation's assurance of a
rollback, not storage the tenant asked for.

## Reading it

`MeteringService.ListUsage`, under `metering.read` over a workload, a project or an organization's
subtree, answers records whose intervals fall in a window (a day up to now by default), paged in
order. Each carries its window, kind, cause, what it supersedes, when it was recorded, and each
quantity it measured, absent where nothing did; the node that measured it is shown only to a caller
who may read that node. A window is read two ways: by its intervals, what was used over them; or
`by_period`, as an invoice sums it, each measurement or gap in the day its interval begins, an
adjustment in the day it was recorded, a record superseded only by an adjustment kept in its closed
day, so a closed day reads the same whenever read and consecutive days count each use once; a host
invoices from the second. `every_version` adds superseded records. `ListPeriods` answers a window's
days (at most a year), each with its state and when it closed or will close, to any caller with a
credential. `korpis get usage` and `korpis get periods` print them.

**What a workload uses now.** A client opens a session through the relay under `metering.read` over
the workload, and the node sends a `UsageNow` at each of its meter's readings: memory in use, and
CPU, throttling, block and network as rates over the window since the reading before. It is not kept
or billed, and the panel shows it beside the billed record rather than reconciling one to the other.
Disk is not among it, being read at interval ends; a burst under ten seconds is averaged into its
window; it is not journaled as a console read is, showing nothing the workload wrote.

## What it costs

Hourly intervals are 24 records a workload a day; one-minute intervals sixty times that, stated when
the interval is chosen. A sample is a journal record every interval for every workload, which the
journal's bound counts. The meter reads cgroup files every ten seconds per object and rewrites its
accounts as often. Memory's integral is as good as ten-second sampling. A machine crash loses up to
ten seconds of each workload's use, recorded as lost. A workload placed and silent costs a row an
interval until heard. A page of an organization's usage joins every record of the window with the
organization tree beneath it.
