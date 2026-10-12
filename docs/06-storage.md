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
  backend           zfs | btrfs | lvm_thin | xfs_project | ext4_project | network
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
| **ZFS** | ✓ atomic | ✓ `send -i` | ✓ `refquota` + inodes | ✓ | ✓ | recommended default |
| **btrfs** | ✓ atomic | ✓ `send -p` | ✓ qgroups | ✓ | ✓ | recommended default |
| **LVM-thin + XFS** | ✓ | ✗ block-level only | ✓ project quota | ✓ | ✗ | acceptable |
| **XFS / ext4 project quota** | ✗ | ✗ | ✓ | ✗ | ✗ | supported, degraded |
| **Network (NFS/iSCSI/Ceph)** | backend-dependent | backend-dependent | backend-dependent | none | none | required for `shared_access` |

The scheduler reads these. A workload whose backup policy requires incremental backup will not be
placed on an `ext4_project` node, because the class cannot deliver it, surfaced at scheduling time
with a reason (§2.1 of `05-scheduling.md`), not discovered on the first slow backup.

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
| ZFS | `refquota` on the dataset | `write()` returns `EDQUOT`, immediately |
| btrfs | qgroup limit on the subvolume | `write()` returns `EDQUOT`, immediately |
| XFS / ext4 | project quota on the project ID | `write()` returns `EDQUOT`, immediately |
| LVM-thin | logical volume size | `ENOSPC` at the block layer |

Three consequences follow, and all three are improvements:

1. **The limit is in the write path.** There is no window, no walk interval, and no race. An 858 GB
   file cannot exist on a 1 GB volume, not because a monitor noticed, but because the kernel
   refused the write.
2. **Usage is read, not computed.** `statfs` inside the tenant's namespace returns current usage in
   constant time. #5186 (the panel misreporting disk usage) cannot occur, because nothing is
   estimating anything.
3. **Inode quotas exist.** A tenant can exhaust a filesystem with millions of empty files without
   using meaningful capacity. `quota_dimensions` includes inodes, and both are enforced. No panel
   in this market does this.

### I/O limits

Ptero's #2798 and the open requests #1871 and #3352: disk I/O and bandwidth are unlimited, so one
tenant degrades a whole node.

Every workload runs in a cgroup v2 with `io.max` set from its `ResourceSpec`:

```
io.max: rbps, wbps, riops, wiops     hard ceilings
io.weight                             share of leftover capacity
```

**And the backup process is throttled too.** This is the actual content of #2798: backups to a
spinning disk cause iowait spikes that degrade every server on the node. In Korpis the backup
worker runs in its own cgroup with a low `io.weight` and explicit ceilings, so it consumes only
capacity tenants are not using. Backup never competes with the workloads it is protecting.

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

### 5.2 What the design gives, and which issue each one closes

| Property | Mechanism | Closes |
|---|---|---|
| Incremental by default | unchanged chunks are already in the repository | #5493 |
| No size limit | chunks are megabytes; multipart is irrelevant | #2599 |
| No local staging | chunks stream directly to the repository | #3846 |
| Doesn't kill the node | throttled cgroup, low `io.weight` | #2798 |
| Deduplicated | content-addressed chunks | none |
| Encrypted before leaving the node | client-side, per-repository key | none |
| Single-file restore | the manifest is a tree; fetch only the needed chunks | none |
| Per-target policy | repositories are objects with their own settings | #4411 |

Deduplication is worth spelling out for this market: fifty game servers running the same modpack
store that modpack's bytes **once**. A provider running five hundred servers from a handful of
popular images stores a small multiple of one server's data, not five hundred times it. This is the
same idea as Rule K-15 and TCAdmin's patch system, applied to the backup path instead of the
distribution path.

Single-file restore falls out for free and is unavailable anywhere in this market: because the
manifest is a tree of chunk references, restoring one config file from three weeks ago fetches a
few kilobytes. Pterodactyl requires downloading and extracting the entire `tar.gz`.

### 5.3 Repositories

```
Repository
  id, name, scope           organization | project
  target                    s3 | b2 | azure | gcs | sftp | local | filesystem
  encryption                key reference, the key is never stored beside the data
  retention                 RetentionPolicy
  dedup_scope               repository
```

```
RetentionPolicy
  keep_last, keep_hourly, keep_daily, keep_weekly, keep_monthly, keep_yearly
  keep_failed_runs   Duration
  min_retention      Duration     a floor no policy or operator can prune below
```

**Deduplication is scoped to the repository, and that is a security boundary, not a tuning knob.**
Content-addressed dedup leaks a bit: an attacker who can write to a repository can test whether a
given file already exists by observing whether their upload deduplicates. Within one organization's
data that is uninteresting. Across tenants it is a confirmation-of-file oracle. The default
repository scope is therefore the organization, and a shared cross-tenant repository is an explicit
operator decision documented with this consequence.

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
- The scope is **visible to every tenant inside it**. A tenant whose backups deduplicate against
  someone else's can see that this is true, because it is a fact about their data and they cannot
  discover it any other way.

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

**Restores are verified.** A restore that completes without confirming the data is readable and the
workload can start is not a restore, it is a hope. The verification is the same contract as
migration's `verify` phase (§8 of `05-scheduling.md`), including symlinks, extended attributes,
sparse files, ACLs, and hardlinks, the omission that makes Pterodactyl's transfers silently produce
unbootable servers (#5429).

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

1. **Encryption key custody.** Client-side encryption means a lost key is lost data, and a key
   stored beside the backups is not encryption. Options: operator-held with an explicit escrow
   ceremony, KMS integration, or per-organization keys wrapped by an operator key. The last
   supports multi-tenancy but makes the operator able to read tenant backups, which may be exactly
   right for a hosting provider and exactly wrong for a shared community. → here, and
   `17-security.md`
2. **Does Korpis ever run an object store?** Backups need a target. Requiring S3-compatible storage
   is one more thing to procure for a single-machine operator; shipping one contradicts "Korpis is
   not a storage provider" (§2 of `00-overview.md`). A local filesystem target is the likely middle
   position, with its limitations stated. → `18-operations.md`
3. **Network storage backends.** `shared_access` is required for VM live migration and for
   failover, but Ceph, NFS, and iSCSI have very different consistency, performance, and failure
   characteristics. Whether one abstraction covers them or each needs its own class is unresolved.
   → here
4. **Backup of the workload's runtime state, not just its disk.** For a `vm` tier with
   `runtime_snapshot`, capturing memory state gives a restore that resumes rather than reboots. It
   also multiplies backup size by the RAM allocation. Probably an opt-in policy rather than a
   default. → here
5. **Cross-node deduplication.** Dedup is currently per-repository, which means a hundred nodes
   backing up identical images each upload them once, a hundred uploads of the same bytes. A shared
   chunk index across nodes would fix it and reintroduces the confirmation-of-file oracle of §5.3
   at a larger scale. → here
