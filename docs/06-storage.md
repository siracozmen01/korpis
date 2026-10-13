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
| **ZFS** | ✓ atomic | ✓ `send -i` | ✓ `refquota` for bytes, project object quota for inodes | ✓ | ✓ | recommended default |
| **btrfs** | ✓ atomic | ✓ `send -p` | ✓ qgroups, bytes only | ✓ | ✓ | recommended default |
| **LVM-thin + XFS** | ✓ | ✗ block-level only | ✓ XFS project quota inside, the thin volume's size outside | ✓ | ✗ | acceptable |
| **XFS / ext4 project quota** | ✗ | ✗ | ✓ | ✗ | ✗ | supported, degraded |
| **Network (NFS/iSCSI/Ceph)** | backend-dependent | backend-dependent | backend-dependent | none | none | required for `shared_access` |
| **Directory** | ✗ | ✗ | ✗ | ✗ | ✗ | a trial, or a machine without project quota |

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
   using meaningful capacity. `quota_dimensions` includes inodes, and both are enforced. No panel
   in this market does this.

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

**One writer per workload prefix, and pruning only in the control plane.** Pruning while a backup
is running can delete a chunk that the running backup has just decided not to upload because it
already existed, which is how deduplicating backup systems lose data. The rule that prevents it is
simple because the keys made it so: a workload's chunks live only under its own prefix, and the
control plane schedules every job that touches a prefix, so it never runs two at once. Backups of
one workload are serialized by the control plane, which schedules them, and a prune of a prefix
takes the same lock. Pruning itself is two-phase and runs with the repository's own
credential, which only the control plane or a relay holds: chunks no manifest references are marked,
and deleted only after a grace period longer than the longest backup job, so even a lock lost to a
crash cannot race a writer into a hole. Recipe content, shared across prefixes, is never pruned
while any manifest in the repository refers to its digest.

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
