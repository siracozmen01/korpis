# Migration

Moving a running workload between nodes as a resumable, verifiable, reversible job with explicit
phases. Pterodactyl's transfer is single-shot and imperative, so an interruption deadlocks a server
into a state needing manual SQL (issue #4505), and symlinks silently vanish (#5429).

## Contents

- [Phases and invariants](#phases-and-invariants)
- [Asking for a migration](#asking-for-a-migration)
- [How each phase advances](#how-each-phase-advances)
- [What crosses](#what-crosses)
- [The receiving rsync's confinement](#the-receiving-rsyncs-confinement)
- [Verify](#verify)
- [Cutover, rollback, release](#cutover-rollback-release)
- [What it costs](#what-it-costs)

## Phases and invariants

```
prepare -> replicate -> quiesce -> final_sync -> verify -> cutover -> release
```

| Phase | Does | Interrupted here |
|---|---|---|
| prepare | the destination accepts the target, fetches content, gets ids and a place per volume | source unaffected; destination cleaned up |
| replicate | copies each volume, pass after pass, while the workload runs | source unaffected; partial copy collected |
| quiesce | stops the workload, or captures live state where the driver can | the source starts it again; abandoned |
| final_sync | sends the last difference, taken stopped | the source starts it again |
| verify | compares manifests; confirms the destination can start it | the source starts it again |
| cutover | the source releases its lease; the destination is granted the next epoch | at most one side holds a valid lease at every instant |
| release | after a hold, the source deletes its copies | source data still present; rollback available |

1. **The source is authoritative until verify passes**, so nothing before cutover can lose the
   workload.
2. **Cutover is a lease epoch change**, so no instant has two authorized nodes: the new epoch is
   granted only after the source released the old one or its term provably ended. How quickly
   traffic follows is a network property, stated on [mesh](mesh.md#what-changes-at-cutover).
3. **Release holds.** Source data is kept for a configurable period (a day by default); if the
   destination fails in production, which is when it will, rollback re-grants the source's lease
   instead of restoring from backup.
4. **Every phase is idempotent and resumable.** An interrupted migration is resumed or rolled back,
   never repaired by hand.

Each phase is a desired state: the control plane sets it in both nodes' assignments, each node
converges to its side and reports, and only then does the control plane advance both.

## Asking for a migration

`MigrateWorkload` (`korpis workload move`) names a workload and optionally a node. With none, the
scheduler chooses among the nodes serving the workload but its own; with one, it is checked as a
manual placement and needs `node.place`. Either way the caller needs `workload.migrate`, implied by
nothing: a grant that may restart a server was never one to take capacity on a second machine. It
proposes a plan of kind `migrate` with one control-plane step, disruptive since quiesce stops the
workload, never approved by a policy: a person approves it, or an operation a person approved. The
plan names the destination with the scheduler's explanation, each volume with its size and how it
crosses, whether quiesce stops or captures, and each endpoint whose address changes.

It is refused, saying why, for a workload holding an address of a `node` pool, one already migrating
or unplaced, a destination failing any placement check, and a local volume whose class cannot cross
(one sending no increments where either helper does not declare `tree_sync`, which a helper declares
only where it finds `/usr/bin/rsync`). The control plane evaluates crossing from the classes both
nodes declared.

Approving it makes a `migration` record when the plan becomes applying (plan, workload, source,
destination, phase, hold, and finally an outcome: completed, rolled back or abandoned, with a
reason), and binds the destination as a second, non-authoritative placement (`bound_by: migration`)
under the placement lock, so its capacity, cores and devices are held from prepare. Approval checks
the destination again as it was chosen: a scheduler's choice against every filter, and only an
operator-named node with its cordon and taints overridden. A workload has one migration at a time,
and while it migrates every other plan but pruning is refused.

## How each phase advances

Both nodes hold an assignment of the workload with a migration role: side, phase, peer, each
volume's crossing with the pass under way and its ticket, and the hold's end. The node not
authoritatively placed (the destination until cutover, the source after) is a standby: target and
volumes, the workload stopped, no plan, endpoints, runs or lease. Each node journals
`MigrationProgress` per phase; a report is believed only from the node assigned that side, about the
current phase, and otherwise recorded as `journal.rejected`.

| Phase | Source | Destination | Done when |
|---|---|---|---|
| prepare | runs on | `Prepare`, content, ids, a place per volume | the destination reports it |
| replicate | sends each volume, pass after pass | receives each pass | every volume's last pass was held and judged small |
| quiesce | stops the workload or captures it; stream writers seal | nothing | the source reports it stopped, with each stream's watermark |
| final_sync | sends what changed since the last pass | receives it | the destination holds the pass |
| verify | computes each manifest | the same, and `Prepare` with the volumes in place | both manifests arrived and are equal, and `Prepare` passed |
| cutover | releases its lease | is granted the next and starts | the placement moved and the destination reports it running |
| release | deletes its copies after the hold | nothing | the source reports them deleted |

**The control plane names and judges every pass.** As replicate and final_sync are entered, each
sent volume gets a pass, an increment from the last one the destination held where the volume
crosses by increments. A pass is small when it sent at most a hundredth of the volume or 64 MiB,
whichever is larger, or when it is the tenth. Once both the source's report of what it sent and the
destination's report that it holds the pass are in, in either order, a held small pass ends that
volume's part of replicate and a held large one starts the next. Neither node reports a copying
phase done: had the source judged smallness, a destination's held report arriving first would start
a pass nobody needed. A pass whose ticket expires before it is held is started again under a new
name. A phase that sends nothing (every volume shared or block) is done as it is entered. A
restarted node takes up the phase its assignment names, and every pass and manifest can be done
again.

## What crosses

Per volume, by its class on the source:

- **A local class that sends increments** (btrfs): the replica transfer, the first pass whole (or an
  increment from a replica the destination already keeps) and each later pass an increment from the
  one before. Before each pass, the volume's own snapshots cross on streams of their own under the
  pass's ticket, oldest first, each an increment from the one before (the source's helper orders
  them by the btrfs transaction that made each); a stream names its snapshot after the ticket, and
  the destination answers whether it holds that one already, so a later pass sends only snapshots
  taken since. They are received beside the passes, in a replica volume of their own, and become
  the volume's own snapshots, under their names, where the last pass becomes the volume.
  While a migration names a volume, replication's release leaves both sides' copies of it alone;
  once the migration ends, completed or abandoned, they are released like any other. The final_sync
  pass is a snapshot taken stopped, and at cutover the destination's last received snapshot becomes
  the volume, as a replica is promoted.
- **Any other local class**: the tree, by rsync. The source's helper runs rsync's daemon side over
  the volume, read only and chrooted to it (`use chroot`, `numeric ids`, no listing); the
  destination's runs the client (`-aHAXS --numeric-ids --super --delete --stats`) pulling into its
  copy of the volume, made at prepare under the destination's identifier, keeping owners by number,
  hardlinks, ACLs, extended attributes and holes, and deleting what the source no longer has. The
  source does not run rsync's client: that would be root reading, unconfined, a tree the running
  workload can change between two of rsync's reads. Each pass sends what differs, so replicate
  converges; a pass from a running workload is inconsistent, which the final pass, taken stopped,
  repairs. At cutover the copy already is the volume. rsync was chosen over tar, which would send
  the whole tree every pass and keep the workload stopped for a whole copy, and over a format of
  Korpis's own, which would be the one place hardlinks, ACLs and holes were read differently from
  every tool that restores them. A node with such a class needs rsync.
- **A shared or block class**: nothing crosses. A shared class's volume is the store's; a block
  device is attached to the destination for the new epoch at cutover by the reconciler that attaches
  it at failover, which fences the source.
- **A stream's volume** crosses as its class's local volumes do, so a console's history travels with
  the workload: a console is evidence, and leaving it on a machine about to be decommissioned
  discards it when it is most wanted. The source's quiesce report names each stream's next offset
  and how far its changes log was journaled; the destination's writer starts at the larger of the
  offset it finds and the one handed over, writing a gap record between them when less arrived than
  was written, and its agent follows the changes log from the handed-over position, so no segment is
  journaled or offloaded twice. Offsets never restart, since stored offsets and readers' resumes
  depend on them.

**Transfers go through the mesh**, from the source's mesh address to the destination's transfer
port, which the destination's table admits from the peer its role names alone. Each pass carries a
ticket the control plane's token key signs over a purpose label of its own (`korpismt_`): the
migration, the volume, both nodes, the pass, its parent, and an expiry a transfer window after the
pass started. The port reads a stream's ticket first, briefly, and hands a migration pass to the
node's migration part, so one port and table serve replicas and migrations alike; each ticket kind
is refused where the other is presented. The destination admits a pass only under a ticket its token
keys open, naming a migration whose destination its role says it is, from the named peer's mesh
address, for a volume and pass its role names. The source sends each pass once, journals what it
sent, and resends every ten seconds one that failed while it is still under way; a failed receive is
not reported, since the source sends it again.

## The receiving rsync's confinement

The destination's rsync client trusts the source's stream as far as rsync's receiver does, and the
receiver has had flaws letting a server write outside the destination (CVE-2024-12087,
CVE-2024-12088) and a heap overflow (CVE-2024-12084). A source node under an attacker's control must
not reach the destination's host through it, so the client runs confined, set up between fork and
exec (`korpis-helper/src/confined.rs`):

- **A view of its own.** A mount namespace whose root is a fresh tmpfs holding, read only, the
  host's `/usr`, `/bin`, `/lib*` and `/sbin` (links kept as links), the loader's cache and the
  helper binary it uses as its remote shell, and, writable, the copy at its own path. No `/etc`
  beyond the cache, no `/proc`, no other volume. Every mount is `nosuid` and `nodev`.
- **Not root.** It runs as a user and group no other running process is, the lowest free id of the
  last range of host ids (which reaches `u32::MAX`, so is never a workload's, and which no host user
  takes), found from `/proc` before the fork; the helper is single-threaded and the child takes the
  id before `spawn` returns. A user that is not root signals no host process, and two receivers are
  never one user.
- **Only the capabilities writing what it receives needs**: `CHOWN`, `DAC_OVERRIDE`, `FOWNER`,
  `FSETID`, `MKNOD`, `SETFCAP`, kept as ambient capabilities through exec, which rsync uses under
  `--super`. Every other capability is dropped from the bounding set, the securebits `NO_ROOT`,
  `NO_SETUID_FIXUP` and `KEEP_CAPS` are locked, and `no_new_privs` is set. Without `SYS_ADMIN`
  nothing in the view mounts, unshares or pivots, which is how root would mount `proc` and walk
  `/proc/1/root` to the host. Device files it writes are unopenable, the copy being `nodev`.
- **Its own network, IPC and host name.** It reaches the source only through the descriptor its
  remote shell inherited.

No rsync version is required of a node: distributions backport the fixes without changing the number
(Ubuntu's `3.2.7-1ubuntu1.5` lists both CVEs), so a version gate would refuse patched builds and
admit nothing safer; the view is the defense either way, and a node's rsync is kept current like its
other packages. **What it does not stop:** a flaw of the receiver can still write anywhere in the
copy it pulls into, which nothing runs on until cutover and verify compares first; and the kernel is
reached through every call those capabilities allow. The view hides the host's files, mounts and
processes, not its kernel.

## Verify

A volume's manifest is a canonical walk: every entry in the byte order of its path, with type, mode,
owner and group by number, extended attributes including ACLs, and for a regular file its size,
modification time, content SHA-256 and holes (from `SEEK_DATA` and `SEEK_HOLE`); for a symlink its
target; for a device node its numbers; for a hardlink its group's first path. Its digest is the
SHA-256 of the walk. The source computes it over what it last sent, stopped, and the destination
over what it received; each also reports the manifest in runs of entries with each run's first path
and digest. The control plane compares the two digests, and where they differ records the first path
of the first differing run and abandons the migration. Shared and block volumes have one copy and
are not compared. This is the check Pterodactyl's #5429 lacked: a lost symlink, attribute or hole is
a difference in the manifest, not in the bytes.

## Cutover, rollback, release

**Cutover is the lease.** The source, stopped since quiesce, releases its lease and journals its
meter's last sample at that instant. On the release, or once the grantor's end of the source's lease
has passed if the source is silent, the control plane moves the placement in one transaction: the
destination's becomes authoritative, and each local volume's record moves to the identifier the
destination's helper gave it. The destination's assignment then carries the power and the stream
watermarks; its agent is granted the next epoch, starts the workload and reports it running. What
follows the placement follows only after the epoch changed, each by its own reconciler: a block
device's attach, a cluster pool's address through its router, a stable endpoint's edges, the ingress
upstreams, the overlay's routes and dependents. A volume whose copy the destination never reported,
or a destination that cannot start the workload, rolls it back.

**Abandon** (before cutover, by a failed phase or `korpis migration abandon`): both roles go, the
source starts the workload again if its power says so, the destination deletes what it received, and
its placement is freed. **Rollback** (after cutover, during the hold): the placement moves back in
one transaction, the destination stops and releases its epoch, and the source is granted the next
epoch over its copies, which hold what they held at quiesce. What the workload wrote on the
destination since cutover is kept as a divergent copy of each volume, deleted only by a plan. A
rollback is refused from a minute before the hold ends, since the source deletes its copies once
the hold ends by its own clock, which may run that far ahead. **Release** (when the hold passes, or
early by `korpis migration release` once the destination reported the workload running): the
source deletes the workload's object and its local copies, its placement is freed and the migration
completes.

**A silent node never stalls a migration.** A silent destination before the move, or a silent source
before cutover, abandons it, so a workload whose source fell silent then fails over normally. A
source silent at cutover is waited out, since its lease's end is what cutover needs. A destination
silent after the move, never having reported the workload started, rolls it back. During release a
silent node is not acted on. A source that never returns leaves the migration in release until the
node is removed.

**The meter is cut at the epoch.** The source's samples are believed while it holds the lease, the
last at its release; the destination's first is under the new epoch and counts from zero; a sample
outside its node's lease is rejected. No interval is counted twice or not at all. The source's
copies during the hold are the installation's rollback assurance, in the source node's allocation
and in neither the meter nor the project's quota.

**Live state.** Where the source's driver declares `runtime_snapshot` and the destination matches
its `restore_requires`, quiesce pauses the workload and captures its state, final_sync carries it,
and the destination restores rather than starts; elsewhere quiesce stops it, and the plan says
which.

## What it costs

Capacity on two nodes from prepare until release, a day by default. The workload is stopped from
quiesce until the destination starts it: the last pass, a manifest that reads every byte on both
sides at disk speed (about forty seconds for 40 GB at 1 GB/s, the price of #5429 not recurring), and
a lease grant. rsync on every node whose local class cannot send increments. Transfers share the
mesh with replication and are not shaped. A migrating workload takes no other plan but pruning
through the hold. It does not move a workload holding a `node` pool's address, move a retained
volume its target does not attach, or move a workload between installations.

**Rebalancing is never automatic.** Migration stops a process, and for a game server that
disconnects players, so a scheduler that moved workloads to improve packing would cause outages
nobody asked for. Rebalancing is an operation an operator asks for and approves with its plan, its
disruption, a `max_unavailable` and a window. The scheduler may recommend; it does not act.
