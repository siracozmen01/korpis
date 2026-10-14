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

**As built, 2026-10-14.** A repository belongs to an organization and serves it whole or one of
its projects; making one takes `repository.write` there and reading one `repository.read` (§5 of
`10-api.md`). It names the organization's backup target provider by name, as every call does, so
a provider need not be serving when the repository is made, and its place in that target by a
relative prefix; each workload's objects are beneath `<prefix>workloads/<workload>/`, which only
that workload's jobs write. A retention policy that keeps nothing is refused, since a prune under
it would remove everything above the floor. `dedup_scope` is not a field: it is the workload,
plus recipe content by reference, and nothing widens it yet.

**Two repositories of an organization never share a place** (Finding 32 of
`23-walkthroughs.md`). A second repository naming the same provider at the same prefix, or at one
that begins with the other's followed by `workloads/`, is refused (`repository.prefix_overlaps`):
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
- **For the same job, it receives a short-lived credential that can write new objects under that
  workload's prefix and nothing else**: an STS session policy on S3-compatible stores, a downscoped
  token on GCS, a user-delegation SAS on Azure, a prefix-restricted application key on B2. It can
  list that prefix, so it can deduplicate against the workload's own history, and it cannot delete
  or overwrite anything.
- **Where a target cannot issue such a credential** (SFTP, most self-hosted stores), the node
  streams its encrypted chunks to a relay (§3.1 of `02-architecture.md`), which holds the target's
  credential and writes them. The relay never holds a DEK, so it moves ciphertext it cannot read.
- **A compromised node therefore reaches the backups of the workloads placed on it**, whose live
  data it could already read, for the length of its jobs, and nothing in any other prefix. That is
  the blast radius §9 of `17-security.md` states, now true of backups as well.

**Who holds the KEK is a declared choice per repository**, because it decides who can read tenants'
backups, and that is exactly the question open question 1 asked:

| `key_custody` | KEK held by | The operator can read tenant backups | Restore needs |
|---|---|---|---|
| `operator` (default) | the control plane, or the operator's KMS | **yes**, through an explicit grant naming the tenant, visible to the tenant (§3.3 of `08-identity.md`) | nothing extra |
| `tenant` | a KMS the tenant controls, which the control plane asks to unwrap per job | **no** | the tenant's KMS to be reachable |

`tenant` custody is the only configuration in which a host genuinely cannot read a customer's
backups, and it costs the customer their own key management and the host the ability to restore
without them. Both are stated where the choice is made.

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
reported it, so a host that cannot apply one shows it as absent (§6 of `17-security.md`).

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
credential, which only the control plane or a relay holds: chunks no manifest references are marked,
and deleted only after a grace period longer than the longest backup job, so even a lock lost to a
crash cannot race a writer into a hole. Recipe content, shared across prefixes, is never pruned
while any manifest in the repository refers to its digest.

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
| `GrantAccess{prefix, mode, ttl}` | the control plane, per job, for the job's node | access to that prefix alone for 60 seconds to 12 hours: `write` puts and lists, never deletes; `read` gets and lists |
| `ListObjects{prefix, after, limit}` | the control plane, pruning | up to a thousand keys beneath the prefix, in key order |
| `DeleteObjects{keys}` | the control plane, pruning | how many of up to a thousand keys it deleted, with its own credential |

Access is S3-shaped (endpoint, region, bucket, the granted prefix's place in the bucket, a
temporary key, secret, and session token, and whether the bucket is addressed in the path or the
host name, which only the provider knows of its store), because the S3 API is what every store in
§5.3 either speaks or fronts, and the node then needs one client, not one per store. A store that
can scope a temporary credential to a prefix (§5.5 names the mechanism for each) is a provider that
issues one. A store that cannot answers `backup.unscoped`, and how a job then reaches it is open
(question 7 below): §5.5 sends such a job through a relay holding the store's credential, and with
the store behind a provider that credential is the provider's alone. The control plane checks every
request before a provider sees it (a relative prefix ending in `/`, a deletion only beneath the
prefix it prunes) and every answer before anything acts on it: access whose place in the bucket is
not the prefix asked for, or a listing that strays outside it, is recorded as `provider.malformed`,
counted by the breaker, and never reaches a node.

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

### 5.7 The repository format

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
system roots (plain HTTP only to a store on the node itself), and sent again, a little later each
time, only when it failed for a reason that passes (the network, a 5xx, a 429); a refusal is final.
It writes and reads only names beneath the granted prefix, and believes a listing only as far as it
stays there. A store whose certificate chains to a private authority needs that authority in the
node's system roots; the access carries no certificate of its own. It is tested against MinIO run
as a host process (`scripts/test-tools.sh`).

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

The content store and the backup repository both use content addressing and are deliberately
**separate systems**. Different trust models (one holds operator-supplied public artifacts, the
other tenant-private encrypted data) different lifetimes, and different failure consequences.
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
8. **Releasing a volume a live workload stopped naming.** Added 2026-10-14, when attaching was
   built (§7). The claim is kept until retirement because the control plane does not know when
   the runtime object that mounted the volume was rebuilt without it. The agent knows: it builds
   every object, from a target, with the mounts it lists. Reporting the volumes each build mounts,
   and releasing a claim once a build of a target without it is reported, would end the cost
   without a window in which two workloads write one directory. → here
9. **Tenant key custody.** Added 2026-10-14, when repositories were built (§5.3). `tenant` custody
   asks a key management service the tenant controls to unwrap a workload's data key per job,
   and no provider type yet reaches such a service, so a repository asking for it is refused with
   `repository.custody_unavailable` rather than made with `operator` custody in its place. It
   needs a `key_management` provider type whose call unwraps one key for one job and whose
   answer the control plane passes to the node without keeping. → `16-extensions.md`
