# Storage, Quotas, Snapshots, and Backup

**Status:** design **Date:** 2026-08-07 **Depends on:** [`01-model.md`](./01-model.md),
[`04-runtimes.md`](./04-runtimes.md), [`05-scheduling.md`](./05-scheduling.md) **Implements:**
Rules K-3, K-5, K-15

---

## 1. One decision, four consequences

Rule K-5 says backup is a consequence of the storage design rather than a feature on top of it.
That is the single most useful idea available in this space, and it is worth stating precisely,
because it determines four things at once:

**The filesystem you choose decides whether you can snapshot. Whether you can snapshot decides
whether backups are instant or slow. Whether the filesystem enforces quotas decides whether the
number in the UI is true. Whether it can send incrementally decides how long a migration takes.**

Pterodactyl made this decision by not making it; it stores server files in a plain directory on
whatever the node's root filesystem happens to be. Every downstream consequence follows: backups
are full `tar.gz` archives every time (#5493 asks for incremental, still open); a 40 GB server
means writing 40 GB, unthrottled, killing the node's iowait for everyone (#2798); backups stage to
local disk before upload, so the node needs free space equal to the backup (#3846); S3 uploads fail
above 5 GB because multipart is not used (#2599); disk usage is computed by walking the directory
tree, so the panel misreports it (#5186); and the quota is advisory, so an 858 GB file can exist on
a server with a 1 GB limit (#4554, closed as *not planned*).

Those are not six bugs. They are one decision, observed six times.

---

## 2. Storage classes declare capabilities

Same pattern as runtime drivers (§4.1 of `04-runtimes.md`): declared, never inferred.

```
StorageClass
  name              string
  backend           zfs | btrfs | lvm_thin | xfs_project | ext4_project | network | directory
  capabilities
    snapshot            bool
    snapshot_atomic     bool     consistent point-in-time without pausing writers
    incremental_send    bool     ship only the delta between two snapshots
    quota_enforced      bool     the kernel fails the write, not a monitor
    quota_dimensions    []       capacity | inodes
    thin_provision      bool
    online_grow         bool
    online_shrink       bool
    reflink_copy        bool     instant copy-on-write clone
    compression         bool
    encryption_at_rest  bool
    shared_access       bool     mountable from more than one node at once
    multi_node_attach   bool     attachable from a different node after the first detaches
    fencing             none | lease_only | storage_enforced
                                 whether the backend itself can refuse a superseded writer
```

> Added 2026-10-13, from the implementation. The helper declared all of this and the control
> plane kept only each class's name and size, so a node's classes said nothing through the API
> about what they could do, and nothing that later reads them (backup planning, the lease rule
> below, a migration's choice of destination) had the declaration to read. The control plane now
> keeps the backend and every capability as declared, refuses a class that names no backend or
> no fencing mode rather than assuming one, and shows them on the node. A class stored before
> this is read as `directory` with nothing, which claims no capability it may lack, until the
> agent's next `Hello` replaces it with the helper's declaration.

`multi_node_attach` is the property that decides lease behaviour, not `shared_access`. A
single-attach Ceph RBD image or iSCSI LUN is never mounted twice at once, but after a reschedule a
second node can attach it, and that is the case in which two writers become possible. Any class
with `multi_node_attach` makes `stop` the derived `on_expiry` default for workloads that use it
(§4.5 of `02-architecture.md`). `fencing: storage_enforced` means the backend refuses the old
writer itself (SCSI-3 persistent reservations, a Ceph exclusive lock with blocklisting, an NFSv4
lease) and the lease epoch is applied there too; `lease_only` means the agents' lease discipline is
the only protection, and the class says so.

| Backend | snapshot | incremental send | quota | thin | reflink | Verdict |
|---|---|---|---|---|---|---|
| **ZFS** | ✓ atomic | ✓ `send -i` | ✓ `refquota` for bytes, project object quota for inodes | ✓ | ✓ | recommended default |
| **btrfs** | ✓ atomic | ✓ `send -p` | ✓ qgroups, bytes only | ✓ | ✓ | recommended default |
| **LVM-thin + XFS** | ✓ | ✗ block-level only | ✓ XFS project quota inside, the thin volume's size outside | ✓ | ✗ | acceptable |
| **XFS / ext4 project quota** | ✗ | ✗ | ✓ | ✗ | ✗ | supported, degraded |
| **Network (NFS/iSCSI/Ceph)** | backend-dependent | backend-dependent | backend-dependent | none | none | required for `shared_access` |
| **Directory** | ✗ | ✗ | ✗ | ✗ | ✗ | a trial, or a machine without project quota |

**As built, 2026-10-14.** The helper offers `directory`, `ext4_project`, `xfs_project`, and
`btrfs` classes, each detected from the filesystem when it starts (§3). ZFS is built when it can be
tested on a host that runs it, which the machine this phase was built on cannot without loading a
kernel module the operator did not ask for; LVM-thin and network classes are not built yet (§4 of
`20-roadmap.md`). A node never offers a backend that is not built: a ZFS dataset under a class
root is a `directory`, enforcing nothing and saying so.

The scheduler reads these. A workload whose backup policy requires incremental backup will not be
placed on an `ext4_project` node, because the class cannot deliver it, surfaced at scheduling time
with a reason (§2.1 of `05-scheduling.md`), not discovered on the first slow backup.

**A directory is a class too, and the least one.** It is a directory on a filesystem with no
project quota: every capability is false, the size and inode count a volume asks for are enforced
by nothing, and its usage is not accounted, because the only way to know it would be the walk of §3.
The helper offers it where a filesystem cannot do better, which is any ext4 or XFS mounted without
`prjquota`, and says so in what it declares. The control plane holds that declaration from the
node's `Hello` (§4.2 of `02-architecture.md`), so the question of whether a volume's size may be
shown has an answer where it is asked: a size is shown only for a volume in a class that declares
`quota_enforced`, and nowhere else (Rule K-3). The cost is real: a tenant on such a class can fill
the machine's disk, and the operator who chose it is told so when choosing it.

> Note, 2026-10-13, from the implementation (`crates/korpis-helper`). The backend list had no
> entry for a filesystem without quota, so a node on one could either claim a class it does not
> have or offer no volumes at all. The development machine the helper was first run on is such a
> node, as are most fresh installs.

**Selecting a degraded class is allowed and is stated plainly.** Choosing ext4 means full backups
forever and slower migrations. That is a legitimate choice for someone with existing hardware. What
is not legitimate is hiding it, the UI names the consequence at the moment of the choice, because
`00-overview.md` §3 P4 forbids claiming what is not enforced, and the same honesty applies to
claiming what is not capable.

---

## 3. Quotas are enforced by the filesystem

Rule K-3. This is the direct answer to #4554.

**How Pterodactyl fails.** Wings computes disk usage by periodically walking the server's directory
tree. That is expensive on millions of files, racy (anything can be written between two walks) and
above all **advisory**: nothing stops the write. The limit is a number in a database compared
against a number produced by a background process. Neither is in the write path.

**How Korpis works.** The quota is a property of the filesystem object backing the volume:

| Backend | Mechanism | Result of exceeding |
|---|---|---|
| ZFS | `refquota` on the dataset; `projectobjquota` for inodes | `write()` returns `EDQUOT`, immediately |
| btrfs | qgroup limit on the subvolume | `write()` returns `EDQUOT`, immediately |
| XFS / ext4 | project quota on the project ID, bytes and inodes | `write()` returns `EDQUOT`, immediately |
| LVM-thin + XFS | XFS project quota inside; the thin volume's size bounds the filesystem | `EDQUOT` from the quota; `ENOSPC` from the filesystem if the volume is full |

**On ext4 and XFS, each volume is a project of its own.** The helper gives the volume's directory
a project identifier that everything created under it inherits, sets the project's byte and inode
limits, and reads its usage back through `quotactl_fd(2)` on the class root, so no block device is
ever named. Identifiers are given from 2^24 upward and recorded beside the volumes, in the class
root, so the record moves with the data and stays clear of the small numbers an operator assigns
by hand on the same filesystem. Which backend a class has is read from the filesystem when the
helper starts, never configured: ext4 or XFS with project quota on is a project class, anything
else a `directory` (§2). Two things follow that the limit does not say. The volume's own
directory counts against it, one inode and one block. And a process holding `CAP_SYS_RESOURCE` in
the host's user namespace passes a hard limit, as the kernel allows root to; a workload never
holds it, because its capabilities are its own user namespace's (§2 of `04-runtimes.md`), but the
helper does, and so does anything else running as root on the host.

**On btrfs, each volume is a subvolume.** The helper creates it with the subvolume ioctl, limits
the bytes it references with its qgroup, and reads usage from the qgroup accounting in the quota
tree, so `statfs` is never asked. btrfs has no inode quota, so a btrfs class declares capacity
alone and an inode limit asked of it is enforced by nothing and shown nowhere (Rule K-3). The class
is btrfs only when qgroups are on and their accounting is consistent when the helper starts:
inconsistent accounting enforces no limit at all, so a class on such a filesystem is a
`directory`, and the helper logs the command that fixes it rather than running it (§2 of
`18-operations.md`: it does not silently modify the host). A usage read made while the accounting
is inconsistent says it is not accounted. Deleting a volume destroys its subvolume and leaves its
empty qgroup: on Linux 6.8 destroying that qgroup marked the whole filesystem's accounting
inconsistent, which lifted every limit until a rescan (`research/evidence.md`), so stale qgroups
are left for an operator's `btrfs qgroup clear-stale`. What it costs: one small item per deleted
volume in the quota tree until then, and a filesystem whose accounting goes inconsistent while the
helper runs keeps its class until the helper restarts, its usage reads saying they are not
accounted meanwhile.

**Thin pools are the one place where exceeding capacity is not a clean error.** A thin volume that
fills returns `ENOSPC` from its filesystem like any full disk. A thin **pool** that runs out of real
blocks underneath its volumes either queues writes or fails them with I/O errors, depending on its
configuration, which is why §5.3 of `05-scheduling.md` stops placing and raises pressure well before
a pool approaches exhaustion. The first version of this table said "`ENOSPC` at the block layer",
which is not what happens at either level (finding S52).

Three consequences follow, and all three are improvements:

1. **The limit is in the write path.** There is no window, no walk interval, and no race. An 858 GB
   file cannot exist on a 1 GB volume, not because a monitor noticed, but because the kernel
   refused the write.
2. **Usage is read, not computed.** Each backend's own accounting returns current usage in constant
   time: `used` against `refquota` on ZFS, the qgroup's referenced bytes on btrfs, the project quota
   through `quotactl` on XFS and ext4. #5186 (the panel misreporting disk usage) cannot occur,
   because nothing is estimating anything. The first version said `statfs` returned it, which is
   true on XFS and ext4 for a directory under project quota and on ZFS, and false on btrfs, where
   `statfs` reports the whole filesystem (finding S52).
3. **Inode quotas exist.** A tenant can exhaust a filesystem with millions of empty files without
   using meaningful capacity. `quota_dimensions` includes inodes, and both are enforced on the
   backends that have an inode quota: ZFS, XFS, and ext4. btrfs has none, so a btrfs class
   declares capacity alone (§2) and an inode limit on it is shown nowhere. No panel in this market
   does this.

### I/O limits

Ptero's #2798 and the open requests #1871 and #3352: disk I/O and bandwidth are unlimited, so one
tenant degrades a whole node.

Every workload runs in a cgroup v2 with its I/O set from its `ResourceSpec` (§5.2 of
`05-scheduling.md`):

```
io.max: rbps, wbps, riops, wiops     hard ceilings, from io.limit
io.weight                             proportional share under contention, from io.share
```

**What the cgroup can and cannot hold, per storage class.** The I/O controller limits I/O it can
attribute to a cgroup. Direct I/O and buffered writeback are attributed on ext4, XFS, and btrfs, so
ceilings hold there. ZFS issues most of its I/O from its own pool threads, so the cgroup sees little
of what a workload causes, and **ZFS classes declare write ceilings unenforced**: the interface does
not display them on ZFS (K-3), and the operator who needs per-tenant I/O isolation chooses a class
that can provide it, knowing why. `io.weight` is a share only when the node runs the `io.cost`
controller with a device model; without it the weight does nothing, so a node without `io.cost`
does not offer shares at all (§2 of `18-operations.md`). There is no I/O floor in cgroup v2, so
nothing in Korpis reserves I/O (finding S38).

**And the backup process is throttled too.** This is the actual content of #2798: backups to a
spinning disk cause iowait spikes that degrade every server on the node. In Korpis the backup
worker runs in its own cgroup with explicit `io.max` ceilings, and with a low share where `io.cost`
is configured, so it consumes only capacity tenants are not using. Where neither applies (a ZFS
class, which cannot attribute the reads either), backups are throttled by their own read rate in
userspace, which is cruder and still bounded.

---

### 3.1 A volume means two different things, and the difference is visible

> External review, finding R16. Recorded in §16 of `23-walkthroughs.md`.

Everything above this line describes a volume as a directory on a filesystem the host controls.
That is true for `process` and `container` and false for `microvm` and `vm`, where the volume is a
block device the guest owns and formats. The driver declares which it is (`volume_semantics` in
§4.1 of `04-runtimes.md`), and the two contracts differ in ways a user will meet:

| | `filesystem` | `block_device` |
|---|---|---|
| Bound | `refquota`, qgroup, or project quota | the device's size |
| Exceeding it | `EDQUOT` in the write path, immediately | `ENOSPC` **inside the guest** |
| Who reports usage | the host, exactly | the guest, if it is asked; the host sees allocated blocks |
| Inodes | a real limit, and a real exhaustion mode | not a host concept at all |
| Resize down | possible within the filesystem | requires the guest to shrink first, often not possible |
| Single-file restore | the manifest is a tree the host can read | the host has an image; reading files inside it needs the guest's filesystem |

None of this is smoothed over. A `microvm` workload's disk usage is reported as **allocated**, and
labelled as such, rather than being presented as the same number a container reports, because it is
not the same number. P4 costs a column in the interface here and buys not having to explain later
why a customer's "20 GB used" meant two different things depending on a field they never set.

The single-file restore of §5.4 is the sharpest case: under `block_device` it needs the guest's
filesystem to be one the restore tooling can read, so the driver declares which image formats and
filesystems it can traverse, and a volume it cannot traverse offers whole-image restore only,
stated at backup time rather than discovered at restore time.

---

### 3.2 Files, as their owner reaches them

> Designed 2026-10-16, for the web client's file manager of Phase 1 (§8 of `11-surface-web.md`).
> The worker that serves it was described in §6 of `02-architecture.md` from its first version;
> what it serves was not.

**A file session is a data-path session** (§3.1 of `02-architecture.md`): opened at a relay with a
capability token for `workload.file.read`, or `workload.file.write`, which implies it, on one
workload, carried to its node, and served there by a filesystem worker the helper starts for the
session, confined as an install's worker is (§4 of `09-recipes.md`): its own mount namespace, the
workload's volumes bind-mounted at their declared paths under an empty root, Landlock to them,
every path resolved beneath that root (`openat2` with `RESOLVE_BENEATH`), no network, and only the
capabilities a file owner needs. It sees the workload's volumes and nothing else, so the session
can name only paths within them, and a symbolic link that points outside one resolves to nothing.
Writing runs as the workload's root inside the workload's user namespace, so what the session
writes is the workload's and counts against the volume's quota in the write path (§3): a full
volume refuses a write with `EDQUOT`, which the session reports as the volume being full. What a
session makes, a file or a directory, is given to the owner of the directory it is made in, so a
workload that runs as a user other than its root can change what was uploaded for it.

> Corrected 2026-10-17, while building the worker. This paragraph said the worker writes "as the
> volume's owner". A workload's volume holds files of several of its users, and the worker serves
> all of a workload's volumes at once, so no single owner exists to run as; running as one would
> also make every file of another user unreadable to the session. The worker runs as the
> workload's root, holding only `CAP_CHOWN`, `CAP_FOWNER`, and `CAP_DAC_OVERRIDE` in a session
> that writes and `CAP_DAC_READ_SEARCH` in one that reads, each over the workload's ids alone,
> and gives what it makes to the directory's owner, which is what "the owner's" was for: a server
> running as uid 1000 owns the plugin uploaded into its directory.

**What a session can do**: list a directory, page by page; read a file's metadata; read a file
from an offset; write to a file at an offset, creating it or truncating it first if asked; make a
directory; rename within the workload's volumes; and remove a file, or a directory with what is in
it. Reads and writes move in chunks of at most a mebibyte, so a large file streams and never sits
whole in the worker, the relay, or the browser. **An upload resumes**: a client that lost its
session reads the size of what it wrote and continues from there, so a 40 GB world survives a
dropped connection, and since the worker writes nothing anywhere but the volume, an upload that is
never finished is a partial file its owner can see and remove, not hidden state.

**What it records.** Opening and ending a session are recorded by the node as console sessions
are, under the chain of the token. Every change is recorded with the session's end, as one effect:
how many files and bytes were written, and every path removed or renamed, up to a thousand, then
their count; a session that ends uncleanly is recorded when the node notices. Reads are not
recorded one by one, except that opening a session under a grant scoped above the workload's
organization, a host reading a customer's files, is the operator access of §7 of
`08-identity.md` and recorded as such.

**What it does not do.** It does not compress or extract archives, set permissions, or follow a
symbolic link out of the volumes; a recipe's install does what a recipe needs (§4 of
`09-recipes.md`), and the rest is for the workload's own tools. It does not reach a volume of a
`microvm` or `vm` workload from the host: there the worker runs in the guest (§6 of
`02-architecture.md`), which Phase 3 builds with those tiers. What it costs: a worker process per
session, started in a few milliseconds and gone at its end; and a session's writes are recorded
when it ends, so an auditor sees a long session's changes when it closes, not as they happen.

> As built so far, 2026-10-17. The session's messages are `files.proto`: one `FileRequest` per
> data frame up, one `FileResponse` per data frame down answering it by its number, and a refusal
> carried as a code (`not_found`, `exists`, `full` for `EDQUOT`, `outside`, `refused`, and the
> rest) that ends nothing. The verbs are `crates/korpis-files`: every path is resolved from the
> root's descriptor with `openat2` and `RESOLVE_BENEATH`, `RESOLVE_NO_SYMLINKS`,
> `RESOLVE_NO_MAGICLINKS`, and `RESOLVE_NO_XDEV`, so a link a tenant planted, a `..`, or a step
> onto another mount reaches nothing, and a link is listed and never followed; a rename never
> replaces what is there; reads and writes move at most a mebibyte and serve only a regular
> file, never waiting on a FIFO the tenant made; a recursive remove goes at most 128 levels
> down; a listing holds a page of names, however large the directory (Finding 79 of
> `23-walkthroughs.md`). The worker is `korpis-fsworker`: the helper's `OpenFiles` verb checks
> the workload's id range against its ledger and each volume path as an install's, starts the
> worker with one end of a socket pair, and answers with the other, so a session's bytes never
> pass through the helper. The worker builds the install worker's root (§4 of `09-recipes.md`),
> confines itself with Landlock to its volumes (their reading alone in a session that reads),
> drops every capability but those above, and serves one length-prefixed `FileRequest` after
> another until the socket closes. Each path is served by the volume mounted at its longest
> prefix, beneath that volume's own descriptor, so a volume inside another is reached as itself;
> the root they are mounted in can be listed and nothing in it changed. Not built yet: the
> node's handler that carries a session from a relay to the worker and records its effect.

## 4. Snapshots are not backups

Conflating these is common and dangerous, so the model separates them.

| | Snapshot | Backup |
|---|---|---|
| Location | same filesystem, same node | remote repository |
| Cost | near zero, copy-on-write | network + storage |
| Speed | instant | proportional to changed data |
| Survives node loss | **no** | **yes** |
| Survives filesystem corruption | **no** | **yes** |
| Use | rollback before an update, source for a backup, migration base | disaster recovery |

A snapshot is a cheap local rollback point and the mechanism that makes backup non-disruptive. It
is not protection against losing the node. Korpis names them differently and never presents a
snapshot as a backup.

**On the node, as built (btrfs).** The helper takes a snapshot as a read-only snapshot subvolume
under the class root's `.snapshots/<volume>/`, a directory no volume identifier can name, so a
snapshot is never mounted into a workload by accident. Rolling a volume back makes it what the
snapshot holds without a moment in which the volume is missing: a writable copy of the snapshot
is made beside the volume, the two are exchanged in one `renameat2(RENAME_EXCHANGE)`, and the
copy, now the old data, is deleted; interrupted anywhere, doing it again ends in the same place.
The volume's byte limit belongs to its subvolume's qgroup, so it is set again on the new one.
Deleting a volume deletes its snapshots, which hold its data too. A class that cannot snapshot
refuses with its backend named. A rollback runs with the workload stopped, which the plan that
carries it arranges (§7).

**In the control plane, as built.** A snapshot is an object of its own (§3.6 of `01-model.md`),
kept from what the node reports it did, never from what was asked. Each operation is a plan on the
volume (§7): `korpis volume snapshot` proposes one of a single `snapshot_volume` step under
`volume.snapshot.create`, refused before any plan exists where the volume's class does not declare
`snapshot`, with the class and its backend named; it changes nothing a workload sees and applies
at once. `korpis volume rollback` proposes, under `volume.snapshot.rollback`, a plan that stops
the holding workload, rolls the volume back, and starts it again when the workload runs, because a
rollback replaces the directory the running workload has open; what was written since is lost, so
the step is irreversible and the plan waits for a person. `korpis volume delete-snapshot` is the
same, under `volume.snapshot.delete`. A snapshot's name is unique among its volume's live
snapshots. What this does not do yet: snapshot on a schedule, keep a retention, or take one by
itself before an update; each is a plan someone, or something holding the authority, proposes.

---

## 5. Backup

### 5.1 The flow

```
1. snapshot the volume                        instant, atomic, workload never pauses
2. read from the snapshot                     a consistent point in time, not a moving target
3. chunk with content-defined boundaries       FastCDC-style rolling hash
4. hash → encrypt → upload only new chunks     dedup happens before the network
5. write a manifest                            a tree of chunk references
6. release the snapshot
```

**The workload never stops.** Step 1 takes milliseconds on ZFS or btrfs; everything after it reads
from a frozen view while the workload continues writing to the live one. Pterodactyl's backup reads
the live directory, which means a 40 GB archive of a running game server is internally inconsistent
by construction, files change while it is being written.

On a class without `snapshot`, Korpis falls back to reading the live tree and **says so** in the
backup record. A backup that might be inconsistent is labelled as such rather than presented as
equivalent.

**As built, 2026-10-14.** `korpis backup create vol_… --repository rep_…` proposes, under
`backup.create` on the volume's workload and `repository.read` on the repository, a plan of one
step, `backup_volume`, which changes nothing the workload sees, loses nothing, and applies at
once. Steps 1 and 6 are inside that step rather than steps of their own: the helper snapshots the
volume for the job where its class can and releases the snapshot when the job is forgotten,
because a step deleting a snapshot is irreversible (§5 of `10-api.md`) and would make every backup
wait for a person. The repository must serve the workload's project; one backup of a workload
into a repository is open at a time (`backup.busy`), which is the one writer per prefix of §5.5;
and the volume's last whole backup in that repository is the new one's parent, whose unchanged
files are not read again (§5.7). The backup is recorded pending with its plan and made whole only
by the report of the node that ran its job; one whose plan ended without such a report reads as
failed, with the reason. When the step begins, the node holding the volume asks for the job's key
and access naming the plan and the step, and the control plane answers only that node, only while
the plan is open and the backup pending: it makes the workload's data key at its first backup
there, opens it, and asks the repository's backup target for twelve hours of write access to the
workload's prefix (§5.5, §5.6). A backup's record is read under `workload.read` (§5 of
`10-api.md`). Nothing here takes a backup on a schedule: a nightly backup is a `scheduled`
workload whose run proposes one under its own grants (Finding 33 of `23-walkthroughs.md`).

**Verified end to end, 2026-10-14**, on a real node: its volumes on a btrfs, its runtime
containerd, its jobs run by the helper, and its repository in a real S3-compatible store (MinIO, a
host process) reached through the first-party S3 target, whose extension the node runs and whose
provider holds the credential the node hands that extension through its identity socket. A running
workload is backed up twice, a prune approved forgets the older backup, and the newer one is
listed, read one file at a time, and cloned into a workload that runs on what it held
(`crates/korpis-e2e/tests/e2e/backups.rs`). Not verified there: ZFS, which needs a host with the
module loaded (§2), and a target other than S3.

### 5.2 What the design gives, and which issue each one closes

| Property | Mechanism | Closes |
|---|---|---|
| Incremental by default | unchanged chunks are already in the repository | #5493 |
| No size limit | chunks are megabytes; multipart is irrelevant | #2599 |
| No local staging | chunks stream directly to the repository | #3846 |
| Doesn't kill the node | throttled cgroup, ceilings and a low share where the class enforces them | #2798 |
| Deduplicated | content-addressed chunks, within each workload's history; recipe content by reference | none |
| Encrypted before leaving the node | client-side, a per-workload data key (§5.5) | none |
| Single-file restore | the manifest is a tree; fetch only the needed chunks | none |
| Per-target policy | repositories are objects with their own settings | #4411 |

Deduplication is worth spelling out for this market, and so is its boundary. Every backup of a
workload deduplicates against that workload's earlier backups, which is what makes nightly backups
of a 40 GB world cost the day's changes. Across workloads, chunks are not shared, because each
workload's chunks are keyed by its own data key (§5.5) so that a node holding one workload's key
learns nothing about another's. What fifty game servers running the same modpack do share is the
modpack: files that are byte-identical to the content of the workload's recipe are recorded in the
manifest **by reference to the recipe's digest** and stored once per repository, so the modpack's
bytes are kept once and every tenant's own world is kept per tenant. This is Rule K-15 applied to
the backup path, bounded by the security rule rather than in spite of it.

> Second external review, finding S37. The first version promised cross-workload deduplication
> with one key per repository, which required every node to hold a key that decrypts every
> workload's backups in that repository. The trade is now the other way round, and the part of the
> saving that matters most in this market, shared game content, survives it.

Single-file restore falls out for free and is unavailable anywhere in this market: because the
manifest is a tree of chunk references, restoring one config file from three weeks ago fetches a
few kilobytes. Pterodactyl requires downloading and extracting the entire `tar.gz`.

### 5.3 Repositories

```
Repository
  id, name, scope           organization | project
  target                    s3 | b2 | azure | gcs | sftp | local | filesystem
  encryption                key-encryption key reference and custody (§5.5); never beside the data
  retention                 RetentionPolicy
  dedup_scope               workload, plus recipe content by reference (§5.2)
  streams                   bool: its workloads' console streams are offloaded here
```

```
RetentionPolicy
  keep_last, keep_hourly, keep_daily, keep_weekly, keep_monthly, keep_yearly
  keep_failed_runs   Duration
  min_retention      Duration     a floor no policy or operator can prune below
```

**The dedup scope is a security boundary, not a tuning knob.** Content-addressed dedup leaks a bit:
an attacker who can write to a repository can test whether a given file already exists by
observing whether their upload deduplicates. Within one workload's history that is uninteresting.
Across tenants it is a confirmation-of-file oracle. Keying chunk identifiers by the workload's own
data key (§5.5) closes the oracle structurally: two workloads storing the same file produce
different chunk identifiers, so nobody can observe the other's. Recipe content is shared by
reference, and recipe content is public by definition, so there is nothing to confirm. The default
repository scope remains the organization, for billing and retention reasons rather than for
secrecy.

**As built, 2026-10-14.** A repository belongs to an organization and serves it whole or one of its
projects; making one takes `repository.write` there and reading one `repository.read` (§5 of
`10-api.md`). It names the organization's backup target provider by name, as every call does, so a
provider need not be serving when the repository is made, and its place in that target by a
relative prefix of letters, digits, `.`, `_`, and `-` between its slashes; each workload's objects
are beneath `<prefix>workloads/<workload>/`, which only that workload's jobs write. A retention
policy that keeps nothing is refused, since a prune under it would remove everything above the
floor. `dedup_scope` is not a field: it is the workload, plus recipe content by reference, and
nothing widens it yet. `streams` says whether the console streams of the workloads it serves are
offloaded to it, kept beneath `<prefix>streams/<workload>/`, beside the workload's backups and
never beneath them, so a prune of the backups never reaches them; §4 of `14-streams.md` says which
repository a workload's stream goes to.

**Two repositories of an organization never share a place** (Finding 32 of
`23-walkthroughs.md`). A second repository naming the same provider at the same prefix, or at one
that begins with the other's followed by `workloads/` or `streams/`, is refused
(`repository.prefix_overlaps`):
the same workload backed up into both would write one prefix under two keys, and each prune would
take the other's objects for garbage. Within an organization a provider name resolves to one
installation, which is what makes the comparison meaningful. Across organizations no check is
needed and none is made, since it would tell one tenant where another keeps its backups: a
workload is only backed up into its own organization's repositories, beneath its own identifier,
so no prefix one organization chooses can put its places inside another's. What this does not
catch is two provider names that the operator pointed at the same bucket and prefix; the control
plane cannot see that, and the provider's configuration is where it has to be avoided.

#### Widening the dedup scope is decided by the people inside it

> Finding 14 of `23-walkthroughs.md`.

"An explicit operator decision" is precise enough with one operator and ambiguous with a delegated
tree, where every parent is an operator of its children. A reseller can put forty customers in one
repository, get a cheaper storage bill, and hand each of those customers an oracle against the
other thirty-nine. The decision is legitimate. The problem is that the party making it is not the
party bearing it, and Korpis knows which is which.

This is Finding 7 of `23-walkthroughs.md` in a different subsystem, and it takes the same answer:
**direction of exposure decides who decides.**

- A repository scoped to one organization is that organization's own decision.
- Widening a scope to span organizations requires the authority of the owner of **every**
  organization it will span, the same rule §4.1 of `16-extensions.md` applies to providers.
- The scope is **visible to every tenant inside it**, because which repository holds a tenant's
  data, and who else's is beside it, is a fact about their data they cannot discover any other way.

With per-workload keys (§5.5) a shared repository no longer shares an oracle, only storage and
public recipe content. The rule above stays, because who decides where a tenant's data lives is
still the tenant's question, whatever the cryptography makes safe.

**Pruning is an irreversible plan step** (§3.3 of `01-model.md`) and always requires approval.
`min_retention` is a floor beneath which no policy, operator, or automation can prune, the defence
against a misconfigured retention rule quietly destroying the history that a ransomware incident
would need.

**What a policy keeps, as built, 2026-10-14.** Each volume's backups are judged on their own, since
a backup is of one volume (Finding 38 of `23-walkthroughs.md`). A whole backup is kept if it is one
of its volume's newest `keep_last`, or the newest of its hour, day, ISO week (Monday to Sunday,
belonging to the year of its Thursday), month, or year, among the newest that many of each that
have a backup, all in UTC: periods without a backup are not counted, so a server that was off for a
week still keeps seven daily backups. So any policy that keeps something keeps each volume's newest
whole backup, which is the parent its next backup takes unchanged files from. Whatever the policy
says, a backup younger than `min_retention` is kept, as is one not yet finished, one an open plan
restores, and one a reader was given access to that has not ended (§5.5); a failed backup's record
is kept for `keep_failed`, so why it failed can still be read. Every decision names the rules that
keep the backup, so a prune's plan shows why each one survives.

### 5.4 Restore

| Mode | Does |
|---|---|
| full | replace a volume's contents from a snapshot in the chain |
| single-file | browse the manifest, fetch only the chunks needed |
| clone | restore into a **new** volume, attach to a new workload, this is how you test a restore |
| point-in-time | any snapshot in the chain, not only the latest |

**Restore into a new volume is the default presentation.** Restoring over a running workload's live
data destroys the current state, which is occasionally what someone wants and frequently a second
disaster on top of the first. The safe operation is the prominent one; the destructive one is
marked irreversible and requires approval.

**As built, 2026-10-14.** `korpis backup restore bkp_… --into <dir>` restores a backup, or one
`--path` in it, into a new directory at the root of a volume, the backup's own unless `--volume`
names another: one `restore_volume` step that loses nothing and applies at once, and refuses a
directory that exists. `korpis backup restore bkp_… --replace` makes the whole volume what the
whole backup held, as a plan that waits for a person: `restore_volume` restores into a staging
place beside the volume while the workload keeps running, then `stop`, `replace_volume`, and
`start`, so the workload is down only while the helper exchanges the two in one rename. The
staging place is a subvolume of its own on btrfs and a quota project of its own where the class
keeps projects, carrying the volume's limit, so the restore is never counted against the live
volume; the exchange keeps the limit, and done again after a crash it changes nothing, because
the staging place's identity, recorded when it was made, says whether it happened. Only a
backup whose record is in the repository restores. The node holding the target volume is given
the backup's own workload key and read access to that workload's prefix, under the same rules as
a backup's job (§5.5). Putting a workload's backup back into it is `backup.restore` there;
putting it into another workload's volume hands its contents to that workload's holders, so it
takes `backup.read` on the first and `workload.file.write` on the second (§5 of `10-api.md`).
`korpis backup ls bkp_… [dir]` browses a whole backup one directory at a time under
`backup.browse`, names, kinds, sizes, and times and no byte of any file; `korpis backup cat bkp_…
<file>` reads one file under `backup.read`, streamed with its holes as zeros and checked against
the digest the backup recorded, so a stream that ends without an error is the file. The control
plane reads these itself, with read access to the workload's prefix and the workload's key,
fifteen minutes of it for a listing and a job's twelve hours for a file (Finding 36 of
`23-walkthroughs.md`), and records each as a sensitive read before it reads anything (§4 of
`15-observability.md`). A declared volume may name `from_backup`, a whole backup it is created
holding: its plan restores the backup into it after `create_volume` and before anything runs on
it, and declaring it takes `backup.read` on the backup's workload, checked only for the volumes a
declaration creates, so a volume made long ago from a backup since pruned is never declared
again. `korpis backup clone` composes that from reads and one declaration: a workload like the
backup's, beside it, which is the clone of the table above. What this did not do at first: keep,
restore, or read recipe content by reference (§5.2), because nothing put a recipe file in a volume
until the install DSL did (§4 of `09-recipes.md`). Since 2026-10-15 a backup is given the
workload's recipe content, the files of its recipe and what its install's `fetch`es downloaded,
each by sha256 and size as the node's content store holds them, and a file byte-identical to one
of them is recorded by reference and stores nothing. A restore reads such a file from the content
store of the node it runs on. Content the node cannot name when the backup starts, because the
registry is unreachable or the store no longer holds it, is left out, and those files are backed
up as any other. The repository keeps its own copy of what backups referenced, once for every
workload in it, beneath `<prefix>content/`, beside the workloads' prefixes and beneath none of
them: a backup job is granted that prefix to write alongside its workload's, keeps there each
content it recorded by reference that the repository lacks, and a restore job, granted it to read,
reads there whatever the node's own store lacks. Content is kept in parts of at most 8 MiB with an
index written last, so a copy whose index exists is whole and neither job holds more than a part
in memory; it is kept unsealed, because recipe content is public, and read back only if it hashes
to the sha256 it is named by. What this does not do: remove content from the repository, which a
prune never touches, so it is kept for the repository's life; record a read of it as sensitive,
since it is public; or stop a node that holds write access from replacing a part, which makes
that content fail its check and its restore refuse it, an availability loss and never a wrong
byte. A copy that fails to be kept leaves the backup whole and restorable wherever the content is
held, and the next backup tries again.

**Replacing a volume needs room for two copies of it.** Until the exchange, the node's filesystem
holds the live volume and the staging place in full, and nothing reserves that room: a restore
that runs out of it fails, and the volume is left as it was. A restore into a directory needs the
same room inside the volume's own limit, and fails the same way (Finding 34 of
`23-walkthroughs.md`).

**Browsing a manifest is a separate action from reading a file.**

> Finding 13 of `23-walkthroughs.md`.

Step 1 of a single-file restore reads the manifest, which is the volume's tree: every path, every
filename, every size, as of that snapshot. Step 3 reads bytes. `08-identity.md` §7 gated the second
and left the first open, on the reasonable-sounding basis that a manifest is not contents.

For a game server the tree is close to harmless. For a database volume, a mail spool, or a
customer's uploads directory it is most of the disclosure, and a support engineer helping someone
find a file should not thereby enumerate forty days of filenames.

So `backup.browse` and `backup.read` are separate actions. Browse is the weaker one, neither
implies the other, and both are recorded as sensitive reads in the audit log (§4 of
`15-observability.md`).

**Single-file restore from a block-device volume runs in a guest kernel.** A `microvm` or `vm`
volume's backup is an image the tenant's guest formatted. Browsing it means parsing that
filesystem, and the host never does: the browse and the fetch run in a disposable service microVM
that attaches the restored image read-only, the same mechanism that gives such volumes file access
when they are stopped (§4.4 of `04-runtimes.md`). A filesystem the service microVM cannot read
offers whole-image restore only, stated when the backup is taken (finding S40).

**Restores are verified.** A restore that completes without confirming the data is readable and the
workload can start is not a restore, it is a hope. The verification is the same contract as
migration's `verify` phase (§8 of `05-scheduling.md`), including symlinks, extended attributes,
sparse files, ACLs, and hardlinks, the omission that makes Pterodactyl's transfers silently produce
unbootable servers (#5429).

### 5.5 Keys, credentials, and who may write

> Second external review, findings S37 and S49. Recorded in §17 of `23-walkthroughs.md`.

The flow of §5.1 encrypts on the node, deduplicates before the network, and streams to the
repository without staging. Each of those needs something on the node: a key to encrypt with, the
chunk index to deduplicate against, and a credential that can write. The first version gave the
node all three for the whole repository while §7 of `17-security.md` said nodes hold no
object-storage credentials, and §9 of the same document bounded a compromised node to "that node's
tenants", which a repository-wide key would have contradicted the moment a repository held more
than one node's workloads.

**Keys form a hierarchy, and a node only ever holds the bottom of it, briefly:**

```
repository key-encryption key (KEK)    held by the control plane, or by an external KMS
  └── workload data key (DEK)          one per workload, generated at its first backup,
                                       stored only wrapped by the KEK
        ├── chunk identifiers          keyed hashes under the DEK: equal content in two
        │                              workloads has unrelated identifiers
        └── chunk and manifest encryption
```

- **For each backup or offload job, the node receives the one workload's DEK**, unwrapped by the
  control plane for that job, and nothing else. It never sees the KEK.
- **For the same job, it receives a short-lived credential that can write, read, and list objects
  under that workload's prefix and nothing else**: an STS session policy on S3-compatible stores, a
  downscoped token on GCS, a user-delegation SAS on Azure, a prefix-restricted application key on
  B2. It lists that prefix to deduplicate against the workload's own history, reads the last
  backup's record and trees to take unchanged files from it, and cannot delete anything; what it
  does not stop of an overwrite is §5.6's. Corrected 2026-10-14 (Finding 41 of
  `23-walkthroughs.md`): this said the credential writes new objects and lists, and cannot
  overwrite, which a backup that reads its parent could not work under, and which §5.6 already
  said it does not ensure.
- **Where a store cannot issue such a credential** (SFTP, a local disk, most self-hosted stores),
  its provider serves the store itself: it holds the store's credential, issues the job a
  credential of its own scoped the same way, and answers the job's requests, so the node writes to
  the provider and the provider to the store (§5.6). The provider never holds a DEK, so it moves
  ciphertext it cannot read. Corrected 2026-10-15 (question 7 of §9): this sent such a job through
  a relay of §3.1 of `02-architecture.md` holding the store's credential, which put a standing
  credential on a relay when backup targets became providers, since the credential is the
  provider's.
- **A compromised node therefore reaches the backups of the workloads placed on it**, whose live
  data it could already read, for the length of its jobs, and nothing in any other prefix. That is
  the blast radius §9 of `17-security.md` states, now true of backups as well.

**Who holds the KEK is a declared choice per repository**, because it decides who can read tenants'
backups, and that is exactly the question open question 1 asked:

| `key_custody` | KEK held by | The operator can read tenant backups | Restore needs |
|---|---|---|---|
| `operator` (default) | the control plane, or the operator's KMS | **yes**, through an explicit grant naming the tenant, visible to the tenant (§3.3 of `08-identity.md`) | nothing extra |
| `tenant` | a KMS the tenant controls, which makes each data key and opens it each time it is used | **not without the tenant's KMS**: every opening is an unwrap it performs, may refuse, and logs; revoking the key ends every opening, the operator's included | the tenant's KMS to be reachable |

> Corrected 2026-10-14, when tenant custody was built (Finding 44 of `23-walkthroughs.md`). The
> table said the operator **cannot** read a tenant's backups under `tenant` custody, and the text
> below it called this the one configuration in which a host genuinely cannot. Neither holds, and
> §8 of `17-security.md` says why in general: the operator runs the nodes. A backup job runs on
> the operator's node with the data key in its memory, as the live volume is on its disk; the
> key management provider is an extension the operator's nodes run, holding a credential for the
> tenant's KMS that the operator could take. What the custody does give is a key that is opened
> only by the tenant's own service, each time, so every opening is in the tenant's KMS log and the
> tenant can end them all, the operator's included, by revoking one key, which makes every backup
> under it unreadable (crypto-shredding). That is a real and checkable property; the old one was
> not.

`tenant` custody costs the customer their own key management and the host the ability to restore
without them. Both are stated where the choice is made.

**Tenant custody, as built, 2026-10-14.** A repository made with `--kms-provider` and `--kms-key`
names the organization's `key_management` provider (§5 of `16-extensions.md`) and the tenant's
key in its service, and has no key-encryption key here. A workload's data key is made by the
tenant's service at its first backup, which keeps it wrapped by the tenant's key; the control
plane stores that wrapping and nothing else. Each job asks for its key naming a one-time X25519
public key it made, and the tenant's service answers with the key sealed to it (HPKE, RFC 9180),
bound to `<repository>/<workload>`, so the key is in the clear only in the job's node and the
tenant's service; never in the control plane, its database, or its call queue, which carry only
what the job alone opens. When the control plane reads a backup itself (browsing, reading one
file, pruning, reading offloaded segments), it makes such a recipient in memory for that one read,
and the tenant's service sees that unwrap like any other. A service that refuses, or cannot be
reached, fails closed: the job is refused and asked again later, and nothing stands in for the key.

What it costs: every backup, restore, browse, read, and prune of such a repository is a call to
the tenant's service, which must be reachable when the work is done, and a revoked key loses every
backup under it, which is the point. What it does not do: keep the operator from reading a backup
while its job runs, which the operator's node holds in the clear; or keep the operator from using
the provider extension's credential to the tenant's service, which runs on the operator's nodes. A
tenant's service should bind each unwrap to the context it is asked for, so a taken credential
opens one workload's keys at a time, and each one in its log.

**The job, as built.** A backup or restore is a `korpis-backup-job` process the helper starts
(`StartJob`) and then only watches (`JobStatus`, `CancelJob`, `ForgetJob`). The supervisor names
the volume, the snapshot, the backup, and for a restore of one path a directory to make at the
volume's root, all as identifiers; the helper turns them into paths, makes a cgroup for the job
beside the workloads' with a tenth of their CPU and I/O weight and ceilings on memory and
processes, and writes the job's spec, the data key and the store's access with it, to the job's
standard input and nowhere else. The job's first act, while it is one thread, is to confine
itself: it joins that cgroup, binds Landlock to its directory (read-only for a backup), its
result's directory, the certificate and resolver files a connection reads, and connections to the
store's port, and drops every capability but its work's from every set, the bounding set included.
A restore is verified (§5.4) before its result says it succeeded. A job outlives a helper that
exits: the helper does not exit idle while one runs, and a restarted helper finds a running job
by the process its pid file names, if that process is still the job binary, and reports one that
ended without a result as lost. What each confinement held is in the result, as the kernel
reported it, so a host that cannot apply one shows it as absent (§6 of `17-security.md`). An
install is a job of the same kind, run by `korpis-install-job` (§4 of `09-recipes.md`).

**The vault, as built.** With `operator` custody the control plane's vault holds the key
hierarchy: a backup key in the data directory (`keys/backup-1.key`, made at first start) seals
each repository's key-encryption key, made with the repository, which seals each workload's data
key, made at its first backup there. Each seal is XChaCha20-Poly1305 with what it is for bound in,
so a sealed key copied to another repository or workload does not open. The database holds only
sealed keys, so a copy of it alone opens no backup; the backup key is what a restore of the
control plane must keep, and keep unrotated, or every backup made under it is lost (§6 of
`18-operations.md`).

**One writer per workload prefix, and pruning only in the control plane.** Pruning while a backup
is running can delete a chunk that the running backup has just decided not to upload because it
already existed, which is how deduplicating backup systems lose data. The rule that prevents it is
simple because the keys made it so: a workload's chunks live only under its own prefix, and the
control plane schedules every job that touches a prefix, so it never runs two at once. Backups of
one workload are serialized by the control plane, which schedules them, and a prune of a prefix
takes the same lock. With prefixes that never coincide (§5.3), one open backup per workload and
repository is one writer per prefix. A prune lists and deletes beneath one workload's prefix at a
time, never beneath the repository's whole prefix, which is what keeps a place another repository
chose inside it out of its reach. Pruning itself is two-phase and runs with the repository's own
credential, which only its backup target's provider holds, the control plane pruning through the
provider's calls (§5.6; corrected 2026-10-15, when this said the control plane or a relay held it):
chunks no manifest references are marked, and deleted only after a grace period longer than the
longest backup job, so even a lock lost to a crash cannot race a writer into a hole. Recipe
content, shared across prefixes, is never pruned while any manifest in the repository refers to its
digest. A backup that an open plan restores is not pruned, and nothing it references is swept,
until that plan ends: a restore reads with its own credential for hours, and the prefix lock is a
writer's, which a reader does not take (Finding 35 of `23-walkthroughs.md`).

**Pruning, as built, 2026-10-14.** `korpis backup prune <workload> --repository rep_…` proposes a
plan of one `prune_backups` step under `backup.prune`. It is irreversible, so it is never approved
by default, and its detail names each backup it forgets and why every other one is kept (§5.3);
nothing is proposed while every backup is kept or being read. The step is the control plane's
(§4.9 of `02-architecture.md`). It forgets the backups the plan names, except one being read by
then, and marks every object beneath the workload's prefix that the backups whose records remain
do not reference. What references what, the control plane reads itself, with read access and the
workload's key, from each remaining record's root tree down (Finding 37 of `23-walkthroughs.md`).
A node is never asked: a host that could name what to delete could delete its own history, which
is what a backup exists to outlive. The sweep is queued for thirteen hours later, an hour longer
than any access a job is given, and begins only when no backup into the prefix is open and no
write access to it is still live (Finding 39 there): every access a job or a reader is given is
recorded before it is asked for, with when it ends, so a job whose plan ended while it ran is
seen. Then it holds the prefix, so a backup proposed meanwhile is refused (`backup.busy`), reads
the references again, deletes what was marked, is still there, and is still unreferenced, and
unmarks the rest. Waiting for a writer spends none of the sweep's attempts.

What it costs: a prune reads every tree of every kept backup twice, once to mark and once to
sweep. Trees are small and shared between backups, but a volume of millions of files is millions
of small reads, through the control plane. What it does not do: prune on a schedule, which waits
for the `scheduled` lifecycle (Finding 33), or anything beneath the repository's own prefix
outside the workload's, which is where recipe content is kept (§5.4), and which outlives every
backup that refers to it.

### 5.6 The backup target interface

> Added 2026-10-13, when the interface was built (§5 of `16-extensions.md`). §5.5 says what a
> node receives and who holds which credential; this is the call that delivers it, and the calls
> pruning runs on.

**A backup target is a provider that holds the target's credential and never hands it out.**
Every object store a repository can name (§5.3) is reached through a provider of type
`backup_target`, an extension like any other (§2 of `16-extensions.md`), so the store's own
credential lives in that extension's configuration and nowhere else. It answers three calls, and
no object byte passes through any of them:

| Call | Asked by | Answers |
|---|---|---|
| `GrantAccess{prefix, mode, ttl}` | the control plane, per job, for the job's node | access to that prefix alone for 15 minutes to 12 hours and never longer than asked: `write` puts, gets, and lists, never deletes; `read` gets and lists |
| `ListObjects{prefix, after, limit}` | the control plane, pruning | up to a thousand keys beneath the prefix, in key order |
| `DeleteObjects{keys}` | the control plane, pruning | how many of up to a thousand keys it deleted, with its own credential |

Access is S3-shaped (endpoint, region, bucket, the granted prefix's place in the bucket, a
temporary key, secret, and session token, and whether the bucket is addressed in the path or the
host name, which only the provider knows of its store), because the S3 API is what every store in
§5.3 either speaks or fronts, and the node then needs one client, not one per store. A store that
can scope a temporary credential to a prefix (§5.5 names the mechanism for each) is a provider that
issues one. A store that cannot is served by its provider itself, below, and a provider that can
neither scope a store's access nor serve it answers `backup.unscoped`. The control plane checks
every request before a provider sees it (a relative prefix ending in `/`, a deletion only beneath
the prefix it prunes) and every answer before anything acts on it: access whose place in the bucket
is not the prefix asked for, or a listing that strays outside it, is recorded as
`provider.malformed`, counted by the breaker, and never reaches a node.

**The secret is in the database for as long as it takes to be read.** A call's answer travels
back through the call queue (§3.3 of `16-extensions.md`), which is a Postgres row, so the
temporary secret is written there. The caller removes it from the row in the transaction after it
reads it, and the sweep removes it from any answer whose caller died, a minute after the answer
came. What that costs: a database reader within that window, or a backup of the database taken
within it, holds a credential that can write one workload's prefix until it expires. That is less
than the database already holds, since with `operator` custody it holds the key-encryption keys
(§5.5), and it is why access is granted per job and never for longer than twelve hours.
What it does not do: make an overwrite impossible. A scoped write credential on S3 can put a key
that exists; chunks are content-addressed, so an overwrite writes the same bytes, and a manifest's
key names its backup, which no other job writes. Where the store supports conditional writes, a
provider should refuse overwrites outright.

**Corrected 2026-10-14** (Finding 41 of `23-walkthroughs.md`), when the first real target was built.
The table said access lasts 60 seconds to 12 hours and that `write` puts and lists. A store's token
service issues nothing shorter than fifteen minutes, so a provider asked for less would have had to
grant more than asked, and a prune's sweep relies on no access outliving what the control plane
recorded (§5.5); and a backup reads its parent's record and trees, which a credential that only puts
and lists refuses, so every backup after a workload's first would have failed. The floor is now
fifteen minutes, which every caller already asked for at least, and `write` reads too. A prefix is
also written into the store's policy, where `*`, `?`, and `${…}` are patterns and variables, so a
repository's prefix is now letters, digits, `.`, `_`, and `-` between its slashes (Finding 42).

**A target can serve its own store.** Added 2026-10-15, with question 7 of §9. A provider in front
of a store that cannot scope access (a disk, SFTP, a self-hosted store without a token service)
answers `GrantAccess` with a credential it minted for the job, scoped to the prefix and the mode
and lasting as asked, and serves the S3 subset a job uses (getting, putting, and listing objects,
never deleting) on an endpoint of its own workload, over TLS. It does not know the address it is
reached at, so its answer names the endpoint, `served_by`, in place of an address, and the control
plane, which checks every answer before anything acts on it, puts in the address. **What evaluates
this, where, and with what information:** the control plane, at the grant, from the installation's
workload's endpoints as placed: the endpoint's node, the port that node holds for it, and the
node's advertised address, which is what every client of a published endpoint reaches (§3 of
`07-networking.md`). A job on any node and the control plane's own reads (a prune's marking, a
backup browsed, an offloaded stream read) reach the store there alike; an endpoint whose exposure
holds no port is refused as `backup.unreachable`. Its certificate is its own, made when it starts,
and the answer carries it: the client trusts that certificate and no other, whatever name or
address it is reached by, which is sound because the answer reaches it only through the provider's
authenticated stream and the control plane. What it costs: every byte of a job passes through one
extension, so the target's throughput is the provider's; and the store is published on a port of
its node, open to the network like any published endpoint, guarded by the signed requests alone, as
an object store on the internet is.

**The filesystem target.** `korpis-target-fs` is the first such provider: a disk of the operator's,
the provider's own volume, as a backup target, for an installation of one machine that has no
object store and for anyone who wants a copy on a disk they hold. Its job credentials live in its
memory alone, so one that restarts has issued none, and a job that held one fails with the reason,
the next being granted access anew, which is also why no access it issued can outlive what the
control plane recorded (§5.5). It lists and deletes for a prune on its own volume. What it does not
do: keep a copy anywhere but that volume, which fails with its disk; a repository on it is a backup
against a workload's mistakes, not against the machine's loss, and the operator who wants the
second keeps a repository on a target elsewhere too. Built 2026-10-15, as `korpis-target-fs`, on
the SDK alone, which signs and verifies requests for it. Its configuration is its workload's
environment, with a default for each variable: where its objects are (its volume, `/data`), where
it listens (port 9000), the endpoint it is served on (`store`), and the name it provides as (`fs`).
Its certificate is an ECDSA P-256 key's, made when it starts. Tested with the node's own client
against it: a job's credential reaches its own prefix as its mode allows and nothing else, and a
store presenting any other certificate, or a request signed with any other secret, is refused; and
on a real node, the provider in a container there, a running workload is backed up twice, pruned,
read by the control plane, and restored into a clone.

**The S3 target, as built, 2026-10-14.** `korpis-target-s3` is the first-party provider for
S3-compatible stores, built on the extension SDK alone, as a third party's would be (§1 of
`22-first-party.md`). Its configuration, read from its workload's environment, is the store's
address, bucket, its own place in the bucket, and its own credential. For each job it calls the
store's token service (`AssumeRole`, signed with its own credential) with a session policy allowing
the mode's actions beneath one prefix and listing only there, for exactly the time asked, and hands
back the temporary credential; it lists and deletes for a prune with its own. It refuses a prefix
that is not plain and a time it cannot honour. Tested against MinIO: a job's credential writes,
reads, and lists its own prefix, reaches no other, deletes nothing, and a restore's reads only; and
a whole backup, restore, and prune run through the control plane on such credentials. What it
needs: its own credential must be allowed to assume the role and to do what the session policy
allows, since the store grants what both allow; on AWS that is a role named by
`KORPIS_S3_ROLE_ARN`, while MinIO assumes the caller's own identity. What it does not do: serve a
store without a token service, such as Backblaze B2's or Google Cloud Storage's S3 interfaces,
which answer no `AssumeRole`; those need their own mechanism (§5.5) or a provider that serves the
store itself, as the filesystem target does.

> Added 2026-10-14, when the backup engine was built (`korpis-backup`, format
> `korpis.backup.v1`).

**Beneath a workload's prefix there are three kinds of object, and every one is sealed.** A chunk
(`chunks/<id>`) is a run of a file's bytes; a tree (`trees/<id>`) is one directory, its entries
sorted by name; a backup (`backups/<id>`) is one backup's record, naming its root tree. Each is
compressed with zstd and then encrypted with XChaCha20-Poly1305 under a fresh random nonce, its
own name bound in as associated data, so an object copied to another name, or offered as another
kind, fails to open rather than being read as what it is not. Every key is derived from the
workload's data key (§5.5) with HKDF-SHA256: one for chunk identifiers and one for tree
identifiers (keyed BLAKE3 hashes, which is what makes the dedup scope of §5.3 structural), one for
a content digest, and one encryption key per kind of object.

**One tree per directory, content-addressed, is the manifest.** A directory nothing in changed
encodes to the same tree, so it is stored once across every backup that contains it, and a
single-path restore (§5.4) fetches the record, the trees on the path, and that path's chunks. A
file's data is read only where the filesystem says it has data (`SEEK_DATA` and `SEEK_HOLE`), and
each run is cut by FastCDC at content-defined boundaries of 512 KiB to 8 MiB, averaging 1 MiB: an
insertion moves the chunks around it and no others, and a sparse file is stored as its data and
restored sparse. A file byte-identical to recipe content is recorded by that content's sha256 and
stores nothing (§5.2). The record is written last, so a backup whose record exists is whole and a
failed one leaves only objects the next backup reuses.

**The next backup reads only what may have changed.** A file whose size, modification time,
change time, and inode number match the parent backup's entry takes that entry's chunks without
being read. The change time cannot be set by the file's owner, so a workload cannot make a changed
file look unchanged; what it costs is that a file changed by something that also restores the
change time, which needs the host's clock, is not read again.

**The walk cannot be steered out of the volume.** Every directory is opened relative to its parent
with `O_NOFOLLOW` and every inode is examined through the descriptor that was opened, so a
workload that swaps a directory for a link while its live volume is read (a class that cannot
snapshot, §5.1) gets the link recorded, never followed; the walk never leaves the volume's
filesystem; and what changed or vanished under it is listed in the record as skipped, which is
what makes a live backup's inconsistency visible rather than silent.

**A restore creates only what it was told, and only where it was told.** Every name is checked
before anything is created from a tree (not empty, not `.` or `..`, no `/` or NUL, each once),
every file is created exclusively relative to a directory the restore created, a whole volume is
restored only into an empty directory, and one path only beside what is there, never over it.
Ownership, then mode, then extended attributes (POSIX ACLs among them), then times are applied in
that order, because each undoes part of the one before it otherwise. Hard links are links again,
and each name of a link group carries the data, so one can be restored alone.

**Verification fetches nothing but trees.** What was restored is walked as a backup would walk it,
sending nothing, and compared entry by entry: kind, mode, owner, modification time, size, content
by its keyed digest (holes read as zeros), link target, device number, extended attributes, and
which names are links of one file. Data is compared by digest because a restoring filesystem may
lay out a sparse file's runs differently from the one it was read from.

**The node's client.** A job reaches the store with that one S3 client: each request presigned for
five minutes with the job's temporary credential, sent over HTTPS verified against the node's
system roots (plain HTTP only to a store on the node itself), or, for a store a provider serves
itself, against the one certificate the access carries (since 2026-10-15), and sent again, a little
later each time, only when it failed for a reason that passes (the network, a 5xx, a 429); a
refusal is final. It writes and reads only names beneath the granted prefix, and believes a listing
only as far as it stays there. A store whose certificate chains to a private authority needs that
authority in the node's system roots; the access carries no certificate of its own unless its
provider serves the store. It is tested against MinIO run as a host process
(`scripts/test-tools.sh`).

**What it does not do.** A socket is recorded and not restored, since nothing listens on it
afterwards. A symbolic link's extended attributes are not recorded: Linux refuses `user.`
attributes on links, and the security labels it allows belong to the host that set them. Anything
holding the data key can write objects that open; that is the node running the workload's job
(§5.5), which could already read the live data. Nothing in the engine deletes: pruning is the
control plane's (§5.5).

---

## 6. The content store

Node-local, content-addressed, shared by every workload on the node (Rule K-15).

```
ContentStore
  entries keyed by digest
    kind        oci_layer | disk_image | recipe_artifact | rootfs_tree
    size, refcount, last_used, verified_at, signature_state
```

- **Fetched once per node.** Fifty workloads from the same image share one copy. This is TCAdmin's
  patch system (download from Steam once, distribute across the network) generalized and automatic.
  Pterodactyl re-downloads the same bytes for every server, forever.
- **Verified on fetch**, against digest and signature, before anything references it.
- **Reflink-shared where the class supports it**: a workload's writable layer is a copy-on-write
  clone of the shared base, so a hundred servers from one image consume one image plus a hundred
  small deltas.
- **Garbage-collected by refcount plus a grace period.** Content stops being referenced constantly
  during normal operation, a workload restarts on a new recipe version and the old one drops to
  zero, and immediately deleting it means re-downloading it on the next rollback.
- **Prefetched on a scheduler hint** (`ContentHint`, §4.2 of `02-architecture.md`), so placing a
  workload on a node that lacks its image is a scheduling cost, not a startup delay.

**Install content, as built, 2026-10-14** (`crates/korpis-agent/src/content/url.rs`). The store
keeps two more things beside images, both by digest. A recipe's files: the node fetches the
recipe's artifact by digest and keeps every layer, and an install finds each file by the path its
title gives (§2 of `09-recipes.md`). And what an install's `fetch` names: the store downloads the
URL once per node, hashing as it writes, keeps it under `sha256` or `sha512` as the recipe declared
only if the hash matches, and deletes it unkept otherwise, so a second workload installing the
same file fetches nothing. The hash is what is trusted, so `http` is allowed; redirects are
followed at most ten times, no proxy is taken from the environment, and a fetch that sends more
than 256 GiB is stopped. What it costs: a hostile URL can still make a node download up to that
bound before the hash refuses it, and a URL that disappears before a node fetches it fails the
install on that node, which is what a fat recipe (§2 of `09-recipes.md`) is for.

> Corrected 2026-10-15, Finding 49 of `23-walkthroughs.md`. What a `fetch` downloaded was kept
> beside everything else, and a fetch whose digest the store already held downloaded nothing. The
> store also holds recipes' files and image layers, pulled from whatever registry a recipe names,
> which may be a tenant's private one; so a recipe naming the digest of another tenant's private
> file, under any URL, would have had it copied into its own volume. Knowing a hash was access.
> What a fetch downloads is now kept apart, in `fetched/`, and a `fetch` reads nothing else: a
> digest a recipe names reaches only bytes some fetch downloaded from a public address. A
> recipe's own files are reached only through that recipe, by the paths its artifact gives.
> What it does not stop: bytes one install fetched from a public URL serve another install that
> names their hash without knowing the URL. That is the deduplication K-15 asks for, and the bytes
> were on the public internet; a recipe that must keep a file private ships it in a private
> recipe, where only its own installs reach it.

The content store and the backup repository both use content addressing and are deliberately
**separate systems**. Different trust models (one holds artifacts by digest, some of them from a
tenant's private registry, the other tenant-private encrypted data) different lifetimes, and
different failure consequences.
Merging them would put tenant data and public images under one garbage collector.

---

## 7. Volume lifecycle

```
provisioned ──▶ attached ⇄ detached ──▶ orphaned ──▶ deleted
                    │
                    └──▶ snapshotted (repeatedly, non-destructive)
```

- **A volume outlives its workload** (§3.6 of `01-model.md`). Deleting a workload detaches; it does
  not delete data. Deletion is a separate, irreversible, approval-requiring plan step.
- **Orphans are listed, never auto-collected.** Same reasoning as orphaned runtime objects (§4.4 of
  `02-architecture.md`): "the control plane forgot about this" and "this should be deleted" are
  indistinguishable from the node's point of view, and one of them destroys customer data.
- **Volumes in a `multi_node_attach` class make `on_expiry: stop` the derived default** for the
  workloads that use them (§4.5 of `02-architecture.md`). Two nodes writing to one network volume
  is corruption, so a workload on such storage must stop when its lease cannot be renewed. Local
  volumes leave the default at `keep_running`, because nothing else can reach that disk.

> Second external review, finding S8. Recorded in §17 of `23-walkthroughs.md`. This rule keyed the
> default on `shared_access`, "mountable from more than one node". The hazard is broader: a
> single-attach network volume is not shared, and it is still attachable by the node a workload is
> rescheduled to, which is exactly when the old node may still be writing. `multi_node_attach`
> names that property, and the rule is now the same here, in §4.5 of `02-architecture.md`, and in
> §8 of `03-state.md`.

**As built (Phase 1).** The control plane keeps the volume object from what nodes report, never
from what it asked for. The agent asks the privileged helper to ensure a volume (a declared one
when a plan creates or attaches it and when the runtime object is built, the console stream's when
the stream is captured, §4 of `14-streams.md`) and journals a volume event after each answer:
the workload, the name it knows the volume by, the helper's identifier for it on the node, the
class, the size asked for, and whether the class enforces that size. Deleting one is journalled
the same way. The record follows the helper's answer, so a volume the journal names was there; one
ensured whose record never reached the journal fails the step or the build that asked for it, and
the retry ensures it again and reports it. The control plane believes a report only from a node
that once held the workload's lease, the same test as for stream segments (Finding 28 of
`23-walkthroughs.md`), because a volume is deleted long after its workload left the node.

From those reports the object is kept: created on its first report in the project of the workload
it was made for, attached to the workload that last ensured it (which is how an adopting workload,
named by `existing`, takes it over), detached in the transaction that retires its workload, and
marked deleted when its node reports the deletion, never removed. A helper identifier names one
live volume on its node; ensured again after a deletion it is a new volume. `VolumeService` lists
a project's volumes, or one workload's, under `volume.read` on the project (deleted ones only when
asked for), and `korpis get volumes` renders them. That listing is where an orphan is seen: a
volume attached to nothing, whose workload is gone, is listed, and nothing collects it.

A size is shown only where the class enforces it in the write path (Rule K-3, §3 of this document):
on a class without quota the API carries no size and the CLI prints none, because a size nothing
enforces is a number the disk will not honour.

**Deletion, as built.** A volume is deleted only by name. Deleting a workload may name some of its
volumes (`--delete-volumes`, §5 of `13-surface-cli.md`); the control plane resolves each name to a
volume its node reported as attached to that workload, refuses a name that is not one rather than
skipping it, refuses a volume on a node other than the workload's, and requires `volume.delete` on
each one beside `workload.delete`. The retire plan then detaches and retains the rest and, after
the runtime that mounted them is removed, deletes each named volume by its own `delete_volume`
step, irreversible by the vocabulary's floor (§3.3 of `01-model.md`), so the plan always waits for a
person. The step carries the helper's identifier as the node reported it, never one derived again
from the name. The node deletes, then journals the deletion, and the volume is marked deleted.

**Plans on a volume.** Deleting a retained volume, and taking, rolling back to, and deleting a
snapshot (§4), are each a plan on the volume rather than a change to a workload, of the workload
holding the volume or of the one that created it when none holds it. One such plan at a time
operates on a volume; a second is refused while one is open, and an approval finds a plan
superseded if the volume went or changed hands meanwhile.

A retained volume no workload holds is deleted by its own plan: `VolumeService.DeleteVolume`
(`korpis volume delete`, §5 of `13-surface-cli.md`) proposes a plan of kind `delete_volume`
whose one step is the same irreversible `delete_volume`, under `volume.delete` on the volume. A
volume a workload holds is refused (it is deleted with that workload), so is one already deleted,
and so is a second plan while one is open. The plan belongs to the workload that created the
volume, whose identifier is kept forever, so its approval, grant, and effects read as any other
plan's do. Its node executes it: through the workload's own assignment if the workload still runs
there, and otherwise through a residual assignment, which carries only that plan and takes no
lease, because nothing on a second node can reach a node-local volume (§4.8 of
`02-architecture.md`). The deletion it reports marks the volume deleted, and the residual
assignment leaves the set once the plan is applied.

> Corrected 2026-10-14, when that deletion was built. This paragraph said a retained volume whose
> workload is gone was not yet deletable, because no assignment reached its node any more. The
> residual assignment is what reaches it. What it costs: a plan that may belong to a retired
> workload, which approval had to learn to accept for this one kind, and an assignment the agent
> must treat as carrying nothing to run.

**Attaching, as built.** A declaration attaches a volume by naming it in a volume's `existing`,
by identifier. The control plane checks, when the intent is declared, that it is a live volume of
the workload's project, that the declarer holds `volume.read` on it (attaching shows the workload,
and so its declarer, everything on it), that it is not a console stream's, and that no other
workload holds it. It then claims the volume for the workload in the transaction that approves
the target, by compare-and-swap, so two declarations naming one volume cannot both be approved;
the second is refused. Placement keeps the workload on the node holding it (§2.1 of
`05-scheduling.md`), and the assignment tells the node the helper's identifier for it, as its node
reported it, so the agent mounts the very directory the first workload wrote. An attached volume
is asked for no size and keeps the limits it has.

What this costs. A volume a live workload's new target no longer names stays attached to that
workload until it is retired: the node mounts it no more after the runtime object is next
rebuilt, but nothing yet tells the control plane when that happened, and releasing the claim
before then would let a second workload mount a directory the first still writes. The volume is
safe and listed, held by a workload that no longer uses it; open question 8 is where that is
settled.

What this does not do yet. Every volume is on the node that reported it, so its `node` is never
empty; a network-backed class (open question 3) is where §3.6 of `01-model.md` says it is, and
that class settles what `node` names for it. The `mode` and `snapshots` of §3.6 are not reported:
every volume is mounted read-write, and snapshots arrive with their own step (§4 of
`20-roadmap.md`).

---

## 8. Interaction with migration

Migration's `replicate` phase (§8 of `05-scheduling.md`) is where the storage class pays off
concretely:

| Class capability | Replicate uses | Relative cost |
|---|---|---|
| `incremental_send` | `zfs send -i` / `btrfs send -p`, block-level delta | fastest, exact |
| `snapshot` only | snapshot, then file-level sync from the frozen view | fast, consistent |
| neither | file-level sync from a live tree | slowest, and inconsistent unless the workload stops |

This is why the storage class is a scheduling input and not merely a storage detail. A cluster on
ZFS can drain a node in minutes with workloads running until cutover. A cluster on ext4 must stop
each workload to move it. Same code, same operation, entirely different operational reality, and
the operator should know which one they bought at the moment they choose the class.

---

## 9. Open questions

1. **Encryption key custody. Resolved in §5.5**: per-workload data keys wrapped by a repository
   key-encryption key, whose custody is a declared choice between the operator and the tenant.
   Client-side encryption means a lost key is lost data, and a key stored beside the backups is not
   encryption. Options: operator-held with an explicit escrow ceremony, KMS integration, or
   per-organization keys wrapped by an operator key. The last supports multi-tenancy but makes the
   operator able to read tenant backups, which may be exactly right for a hosting provider and
   exactly wrong for a shared community, which is why it is now a choice rather than a default.
2. **Does Korpis ever run an object store? Resolved here: no.** Backups need a target. Requiring
   S3-compatible storage is one more thing to procure for a single-machine operator; shipping one
   contradicts "Korpis is not a storage provider" (§2 of `00-overview.md`). A local filesystem
   target is the likely middle position, with its limitations stated. It was routed to
   `18-operations.md`, which never carried it (finding S18).

The `filesystem` target is supported for exactly that operator, on a separate disk or a mounted
remote filesystem, and a target on the same machine as the data it protects is labelled **not
off-site** wherever its backups are shown, because it does not survive the machine. Anything
S3-compatible, from a commercial service to a self-hosted store the operator runs themselves,
serves the rest. Korpis ships no object store and does not become one.
3. **Network storage backends.** `shared_access` is required for VM live migration and for
   failover, but Ceph, NFS, and iSCSI have very different consistency, performance, and failure
   characteristics. Whether one abstraction covers them or each needs its own class is unresolved.
   → here
4. **Backup of the workload's runtime state, not just its disk.** For a `vm` tier with
   `runtime_snapshot`, capturing memory state gives a restore that resumes rather than reboots. It
   also multiplies backup size by the RAM allocation. Probably an opt-in policy rather than a
   default. → here
5. **Cross-node deduplication. Resolved by §5.2 and §5.5, and the question was framed wrongly.** It
   said dedup was per repository and that a hundred nodes would each upload identical images. Dedup
   per repository already spans every node writing to it, so the real question was always across
   workloads and tenants, which is where the oracle lives (finding S49). The answer is that
   identical *recipe* content is stored once per repository by reference, which covers identical
   images and modpacks, and identical *tenant* content is not shared, deliberately, because sharing
   it would require a key that reaches every tenant's data.
6. **Hibernated volumes and cheaper storage.** A hibernated workload keeps its volume, so it still
   consumes disk and still counts against quota. Whether hibernated volumes move to cheaper storage
   automatically is attractive and interacts with §7. Routed here from §9 of `22-first-party.md`
   (question 2) and not carried until now (finding S18). A move is a migration of the volume
   alone, with every guarantee of §8 of `05-scheduling.md`, and waking would then pay for the move
   back; whether that trade is worth automating is a measurement, not a principle. → here
7. **A store that cannot scope access, behind a provider.** Added 2026-10-13, when the backup
   target interface was built (§5.6). §5.5 sends a job for an SFTP or self-hosted store through a
   relay that holds the store's credential, because the node must not. Behind a provider, the
   credential is the provider's, and the relay is not the provider. Either the provider grants the
   relay whole-store access, which puts a standing credential on a relay §5.5 meant to hold one, or
   the provider accepts the relay's ciphertext stream and writes it, which makes the provider a
   data path and needs it reachable from the relay. The second keeps every credential where it is
   and costs throughput through one extension; it is the likelier answer, decided with the relay
   tier (§3.1 of `02-architecture.md`) and the backup flow it serves. → here
   Resolved in §5.6, 2026-10-15: the second. The provider serves the store's S3 subset itself, with
   credentials it mints per job, and the job's node reaches it at the endpoint the control plane
   resolves; the relay is not involved, and §5.5's bullet is corrected in place.
8. **Releasing a volume a live workload stopped naming.** Added 2026-10-14, when attaching was
   built (§7). The claim is kept until retirement because the control plane does not know when
   the runtime object that mounted the volume was rebuilt without it. The agent knows: it builds
   every object, from a target, with the mounts it lists. Reporting the volumes each build mounts,
   and releasing a claim once a build of a target without it is reported, would end the cost
   without a window in which two workloads write one directory. → here
9. **Tenant key custody. Resolved in §5.5**, 2026-10-14: a `key_management` provider type whose
   calls make and open one data key for one context, sealed to a one-time recipient, never in the
   clear; the claim the question was asked under is corrected there. Added 2026-10-14, when
   repositories were built (§5.3). `tenant` custody
   asks a key management service the tenant controls to unwrap a workload's data key per job,
   and no provider type yet reaches such a service, so a repository asking for it is refused with
   `repository.custody_unavailable` rather than made with `operator` custody in its place. It
   needs a `key_management` provider type whose call unwraps one key for one job and whose
   answer the control plane passes to the node without keeping. → `16-extensions.md`
