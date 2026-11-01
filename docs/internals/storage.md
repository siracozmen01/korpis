# Storage

Storage classes, how a volume's limits are enforced, what a volume is on each tier, snapshots, the
volume lifecycle, and what a class gives migration. Storage reached over the network is on
[network storage](network-storage.md), replicated volumes on [replication](replication.md), file
sessions on [files](files.md), and backup on [backup](backup.md).

## Contents

- [One decision, four consequences](#one-decision-four-consequences)
- [Storage classes](#storage-classes)
- [Quotas are enforced by the filesystem](#quotas-are-enforced-by-the-filesystem)
- [I/O limits](#io-limits)
- [Two kinds of volume](#two-kinds-of-volume)
- [Snapshots are not backups](#snapshots-are-not-backups)
- [Volume lifecycle](#volume-lifecycle)
- [What a class gives migration](#what-a-class-gives-migration)

## One decision, four consequences

Backup is a consequence of the storage design rather than a feature on top of it. The filesystem
decides whether a volume can be snapshotted; snapshots decide whether backups are instant or slow;
whether the filesystem enforces quotas decides whether the number in the panel is true; and whether
it sends incrementally decides how long a migration takes.

Pterodactyl made this decision by not making it: server files in a plain directory on whatever the
node's root filesystem is. Every consequence follows: backups are a full `tar.gz` every time
(pterodactyl/panel#5493); a 40 GB server means writing 40 GB unthrottled, raising every tenant's
iowait (#2798); backups stage to local disk first (#3846); S3 uploads fail above 5 GB without
multipart (#2599); usage is computed by walking the tree, so the panel misreports it (#5186); and
the quota is advisory, so an 858 GB file can exist on a 1 GB server (#4554, closed as not planned).
That is one decision observed six times.

## Storage classes

A class declares its capabilities, as a runtime driver does, and nothing infers them.

```
StorageClass
  name
  backend          zfs | btrfs | lvm_thin | xfs_project | ext4_project | network | directory
  capabilities
    snapshot, snapshot_atomic     a consistent point in time without pausing writers
    incremental_send              ship only the delta between two snapshots
    tree_sync                     the helper can send and receive the tree by rsync
    quota_enforced                the kernel fails the write, not a monitor
    quota_dimensions              capacity | inodes
    thin_provision, online_grow, online_shrink, reflink_copy, compression, encryption_at_rest
    shared_access                 mountable from more than one node at once
    multi_node_attach             attachable from another node after the first detaches
    fencing                       none | lease_only | storage_enforced
```

The control plane keeps every class's backend and capabilities as its node's helper declared them,
refuses a class naming no backend or fencing mode rather than assuming one, and shows them on the
node. Placement, backup planning, the lease rule and migration read them.

| Backend | Snapshot | Incremental send | Quota | Thin | Reflink | Verdict |
|---|---|---|---|---|---|---|
| ZFS | atomic | `send -i` | `refquota`; project object quota for inodes | yes | yes | recommended |
| btrfs | atomic | `send -p` | qgroups, bytes only | yes | yes | recommended |
| LVM-thin with XFS | yes | block-level only | XFS project quota inside, the thin size outside | yes | no | acceptable |
| XFS or ext4 project quota | no | no | yes | no | no | supported, degraded |
| network | by kind | no | by kind | no | no | [network storage](network-storage.md) |
| directory | no | no | no | no | no | a trial, or a filesystem without project quota |

The helper offers `directory`, `ext4_project`, `xfs_project` and `btrfs`, each detected from the
filesystem at start, never configured, and the network classes. ZFS and LVM-thin are not built yet
(ZFS needs a host where its kernel module may be loaded), and a node never offers a backend that is
not built: a ZFS dataset under a class root is a `directory`, enforcing nothing and saying so.

**`multi_node_attach` decides lease behaviour, not `shared_access`.** A single-attach RBD image or
iSCSI LUN is never mounted twice at once, but after a reschedule a second node can attach it, which
is when two writers become possible. So any class with `multi_node_attach` makes `stop` the derived
`on_lease_expiry` of workloads using it (see [node agent](node-agent.md)). `storage_enforced`
fencing means the backend refuses the old writer itself (SCSI-3 persistent reservations, a Ceph
exclusive lock with blocklisting), the lease epoch applied there too; `lease_only` means the agents'
lease discipline is the only protection, and the class says so. An NFSv4 lease is not storage
fencing: it is between a client and its server, renewed by the client's own requests (RFC 7530,
section 9.5), so a node cut off from the control plane but not from the server keeps writing.

**The scheduler reads these.** A workload whose backup policy needs incremental backup is not placed
on an `ext4_project` node, with a reason at [placement](placement.md), rather than discovered on the
first slow backup.

**A directory is a class too, and the least one.** Every capability is false, the size and inode
count a volume asks for are enforced by nothing, and usage is not accounted, since the only way to
know it would be a walk. The helper offers it where nothing better is possible: ext4 or XFS mounted
without `prjquota`, btrfs with qgroups off or inconsistent. A size is shown only for a volume in a
class declaring `quota_enforced`, nowhere else. The cost is real: a tenant on such a class can fill
the machine's disk, and the operator choosing it is told so then. Choosing a degraded class (ext4
means full backups forever and slower migrations) is legitimate; hiding it is not, so the panel
names the consequence at the moment of the choice.

## Quotas are enforced by the filesystem

Pterodactyl's Wings computes usage by periodically walking the server's tree: expensive on millions
of files, racy, and above all advisory, since nothing stops the write. In Korpis the quota is a
property of the filesystem object backing the volume:

| Backend | Mechanism | Exceeding it |
|---|---|---|
| ZFS | `refquota` on the dataset; `projectobjquota` for inodes | `EDQUOT`, immediately |
| btrfs | qgroup limit on the subvolume | `EDQUOT`, immediately |
| XFS, ext4 | project quota, bytes and inodes | `EDQUOT`, immediately |
| LVM-thin with XFS | XFS project quota inside; the thin volume's size bounds the filesystem | `EDQUOT`, or `ENOSPC` when the volume is full |

- **The limit is in the write path**: no window, walk interval or race. An 858 GB file cannot exist
  on a 1 GB volume because the kernel refused the write.
- **Usage is read, not computed**, from each backend's own accounting in constant time: `used` on
  ZFS, the qgroup's referenced bytes on btrfs, the project quota through `quotactl` on XFS and ext4.
  Not `statfs`, which on btrfs reports the whole filesystem.
- **Inode quotas exist** where the backend has one (ZFS, XFS, ext4), since a tenant can exhaust a
  filesystem with millions of empty files. btrfs has none, so a btrfs class declares capacity alone
  and an inode limit on it is shown nowhere.

**On ext4 and XFS, each volume is a project of its own.** The helper gives the volume's directory a
project id everything beneath inherits, sets byte and inode limits, and reads usage through
`quotactl_fd(2)` on the class root, so no block device is ever named. Ids are given from 2^24 upward
and recorded in the class root beside the volumes, so the record moves with the data and stays clear
of ids an operator assigns by hand. The volume's own directory counts against it, one inode and one
block. A process with `CAP_SYS_RESOURCE` in the host's user namespace passes a hard limit, as the
kernel allows root to; a workload never holds it, the helper and anything else root on the host do.

**On btrfs, each volume is a subvolume**, made with the subvolume ioctl, its referenced bytes
limited by its qgroup and read from the quota tree. The class is btrfs only when qgroups are on and
consistent at the helper's start: inconsistent accounting enforces no limit, so such a filesystem is
a `directory`, and the helper logs the command that fixes it rather than modifying the host.
Deleting a volume leaves its empty qgroup, because on Linux 6.8 destroying it marked the whole
filesystem's accounting inconsistent, lifting every limit until a rescan; stale qgroups are left for
an operator's `btrfs qgroup clear-stale`. A filesystem going inconsistent while the helper runs
keeps its class until a restart, its usage reads saying they are not accounted.

**A thin pool is the one place where exceeding capacity is not a clean error.** A full thin volume
returns `ENOSPC` like any disk, but a pool out of real blocks queues writes or fails them with I/O
errors, by configuration, which is why [quota](quota.md) and placement stop placing and raise
pressure well before a pool nears exhaustion.

## I/O limits

Disk I/O and bandwidth unlimited means one tenant degrades a node (pterodactyl/panel#2798, #1871,
#3352). Every workload runs in a cgroup whose `io.max` (read and write bytes and operations per second)
comes from `io.limit` and whose `io.weight` comes from `io.share`. The controller limits I/O it can
attribute to a cgroup: direct I/O and buffered writeback are attributed on ext4, XFS and btrfs, so
ceilings hold there; ZFS issues most of its I/O from its own pool threads, so ZFS classes declare
write ceilings unenforced and the interface does not show them there. `io.weight` is a share only
with the `io.cost` controller and a device model, so a node without it offers no shares. There is no
I/O floor in cgroup v2, so nothing reserves I/O. See [resources](resources.md#io-and-processes).

**Backups are throttled too**, which is the real content of #2798: a backup job runs in its own
cgroup with `io.max` ceilings and a low share where `io.cost` is configured, so it uses only
capacity tenants are not using; where neither applies (ZFS), by its own read rate in userspace,
cruder and still bounded.

## Two kinds of volume

A volume is a directory on a filesystem the host controls for `process` and `container`, and a block
device the guest owns and formats for `microvm` and `vm`. The driver declares which
(`volume_semantics`), and the two contracts differ in ways a user meets:

| | `filesystem` | `block_device` |
|---|---|---|
| Bound | `refquota`, qgroup or project quota | the device's size |
| Exceeding it | `EDQUOT` in the write path | `ENOSPC` inside the guest |
| Usage | the host, exactly | the host sees allocated blocks |
| Inodes | a real limit | not a host concept |
| Shrinking | within the filesystem | needs the guest to shrink first, often impossible |
| Single-file restore | the host reads the backup's tree | a service guest reads the image |

None of this is smoothed over: a guest's disk usage is reported as allocated and labelled so
(`disk_allocated` in metering; "allocated" in `korpis get usage` and the panel), because "20 GB
used" must not mean two things depending on a field the customer never set. Allocated blocks grow as
the guest writes and do not shrink when it deletes.

**A block device is a volume holding one image.** It is a directory or btrfs subvolume in its class,
in its own quota project or qgroup, holding `block.ext4`. The agent asks for one when the driver
that will run the workload declares block devices. The helper makes the image sparse at the volume's
size, formats it empty with `mke2fs`, its root owned by the workload's root, and never mounts or
reads it again: what the guest writes is parsed by the guest's kernel alone. A larger size extends
the image and the guest grows the filesystem online at its next mount; a smaller one is refused. The
quota leaves room for the image's extent blocks, and the enforced bound reported is the image's
size. A volume's kind is the extended attribute `trusted.korpis.volume`, valued `block_device`,
which no workload can set, and a volume is never made the other kind: a workload moved from
`container` to `microvm` keeps its files and is refused the block device. A block device is attached
to one workload at a time, since two kernels mounting one filesystem corrupt it. Snapshots, backups
and whole-volume restores take the directory as any other.

## Snapshots are not backups

| | Snapshot | Backup |
|---|---|---|
| Location | same filesystem, same node | a remote repository |
| Cost | near zero, copy-on-write | network and storage |
| Survives node loss or filesystem corruption | no | yes |
| Use | rollback before an update, a backup's source, a migration base | disaster recovery |

Korpis names them differently and never presents a snapshot as a backup.

**On the node (btrfs).** A snapshot is a read-only subvolume under the class root's
`.snapshots/<volume>/`, which no volume id can name, so it is never mounted into a workload by
accident. Rolling back makes the volume what the snapshot holds with no moment in which it is
missing: a writable copy is made beside the volume, the two exchanged in one
`renameat2(RENAME_EXCHANGE)`, and the old data deleted; interrupted anywhere, doing it again ends in
the same place. The byte limit is the subvolume's qgroup's, so it is set again on the new one.
Deleting a volume deletes its snapshots. A class that cannot snapshot refuses, naming its backend.

**In the control plane**, a snapshot is an object kept from what the node reports, never from what
was asked. `korpis volume snapshot` proposes a plan of one `snapshot_volume` step under
`volume.snapshot.create`, refused before any plan where the class cannot snapshot, and applies at
once. `korpis volume rollback`, under `volume.snapshot.rollback`, stops the holding workload, rolls
back and starts it again, since a rollback replaces the directory a running workload has open; what
was written since is lost, so the step is irreversible and waits for a person. `delete-snapshot` is
the same under `volume.snapshot.delete`. Names are unique among a volume's live snapshots. Nothing
snapshots on a schedule, keeps a retention, or snapshots by itself before an update; each is a plan
someone, or something holding the authority, proposes.

## Volume lifecycle

```
provisioned -> attached <-> detached -> orphaned -> deleted
                   \-> snapshotted, repeatedly
```

- **A volume outlives its workload.** Deleting a workload detaches; deletion of data is a separate,
  irreversible step a person approves.
- **Orphans are listed, never collected.** "The control plane forgot this" and "this should be
  deleted" are indistinguishable from a node, and one destroys customer data.
- **Local volumes leave `on_lease_expiry` at `keep_running`**, since nothing else can reach that
  disk, unless the volume is replicated and its workload declares automatic replica failover, when
  another node would start it on the replica.

**The volume object is kept from what nodes report.** The agent asks the helper to ensure a volume
(when a plan creates or attaches it, when the runtime object is built, when a console stream is
captured) and journals a volume event after each answer: the workload, its name for the volume, the
helper's id for it, the class, the size asked and whether the class enforces it. A volume the
journal names was there; one ensured whose record never reached the journal fails the step that
asked, and the retry reports it. The control plane believes a report only from a node that once held
the workload's lease, since a volume is deleted long after its workload left. The object is created
on its first report, attached to the workload that last ensured it, detached when that workload
retires, and marked deleted when its node reports the deletion, never removed. `korpis get volumes`
lists them under `volume.read`, which is where an orphan is seen.

**Deletion is only by name.** Deleting a workload may name some of its volumes (`--delete-volumes`);
each must resolve to a volume its node reported attached to that workload, on the workload's node,
under `volume.delete` beside `workload.delete`. The retire plan detaches and retains the rest and,
once the runtime is removed, deletes each named volume by its own irreversible `delete_volume` step,
carrying the helper's id as the node reported it, never one derived again from the name.

**Plans on a volume.** Deleting a retained volume and taking, rolling back or deleting a snapshot
are each a plan on the volume, of the workload holding it or, when none does, the one that created
it, whose id is kept forever. One such plan at a time operates on a volume, and approval finds a
plan superseded if the volume went or changed hands. A retained volume no workload holds is deleted
by `korpis volume delete`, executed by its node through the workload's own assignment if it still
runs there, and otherwise through a residual assignment, which carries only that plan and takes no
lease, since nothing on another node can reach a node-local volume.

**Attaching.** A declaration attaches a volume by naming its id in `existing`. At declaration the
control plane checks it is a live volume of the workload's project, that the declarer holds
`volume.read` on it (attaching shows the workload everything on it), that it is not a console
stream's, and that no other workload holds it; it then claims the volume in the transaction
approving the target, by compare-and-swap, so two declarations cannot both be approved. Placement
keeps the workload on the node holding it, and the assignment names the helper's id, so the agent
mounts the very directory the first workload wrote. An attached volume keeps its limits. The cost: a
volume a live workload's new target no longer names stays claimed until that workload retires, since
nothing yet tells the control plane when the runtime object was rebuilt without it, and releasing
earlier would let a second workload mount a directory the first still writes.

A size is shown only where the class enforces it in the write path; on a class without quota the API
carries none.

## What a class gives migration

| Class capability | A migration's copy uses | Cost |
|---|---|---|
| `incremental_send` | `zfs send -i` or `btrfs send -p`, as a [replica](replication.md) is sent, from a replica the destination keeps where there is one | fastest, exact |
| `snapshot` only | a snapshot, then file-level sync from the frozen view | fast, consistent |
| neither | rsync passes while the workload runs, then once stopped | slowest |
| shared or block | nothing: the device is attached to the destination at cutover | none |

So the storage class is a scheduling input, not a storage detail: a cluster on ZFS drains a node in
minutes with workloads running until cutover, one on ext4 stops each workload to move it, and the
operator learns which at the moment of choosing the class. What arrived is compared, not trusted;
see [migration](migration.md).
