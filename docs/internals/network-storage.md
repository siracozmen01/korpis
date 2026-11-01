# Network storage

Storage a node reaches over the network. What separates it for Korpis is not the vendor but what a
node is given: a filesystem many nodes mount at once (NFS, CephFS), or a block device one node
attaches at a time (a Ceph RBD image, an iSCSI LUN, an NVMe/TCP namespace). Consistency, fencing,
quota and failure differ between those two far more than between vendors within each, so there are
two kinds of `network` class, by `access`, and no class per vendor. Both declare
`multi_node_attach`, so a workload on either has `stop` as its derived `on_lease_expiry`; only the
first declares `shared_access`. Local classes are on [storage](storage.md).

## Contents

- [Shared filesystem classes](#shared-filesystem-classes)
- [Which nodes share a store](#which-nodes-share-a-store)
- [Block classes](#block-classes)
- [Devices in the control plane](#devices-in-the-control-plane)
- [Devices on the node](#devices-on-the-node)

## Shared filesystem classes

The operator mounts the filesystem on every node offering the class, as they mount a local disk, and
names the directory to the helper (`--shared-class NAME=PATH`); Korpis mounts nothing, holds no
export credential and chooses no mount option. The helper refuses the class at start unless the
filesystem is reached over the network (by `statfs` type NFS, CephFS or SMB, or a FUSE mount typed
`ceph-fuse` or `glusterfs`), since a local filesystem named shared would promise a second node sees
what the first wrote with nothing keeping the promise; and unless it can make a directory there,
give it away and remove it, which an export squashing root forbids. A volume is a directory of the
class.

The class declares `shared_access`, `multi_node_attach` and `fencing: lease_only`, and nothing else:
no snapshot, incremental send or quota. NFS has no per-directory quota, and CephFS's quotas are kept
by the mounting client, which stops writers some seconds late and not at all if modified, which is a
monitor and not the write path; so a volume's size in a shared class is shown nowhere.

A volume in a shared class has no node: its `store` names the storage, and the placement check that
a node holds every volume a workload attaches is met by any node offering a class of that store. Its
space is the store's, counted once, the store's capacity and free space being what its nodes last
reported. A node's disk, which placement counts, leaves its shared classes out. An attached volume's
declaration records its class, and naming another is refused, since the node must look for it there
and not make an empty one in its default class. The drivers mount only what is under the volume root
or a `--shared-root`, checked on the filesystem.

**What it costs.** Nothing enforces a volume's size, so a tenant can fill the store, and the
operator offering it is told so. Fencing is the agents' alone: a node cut off from the control plane
but not from the store writes until its lease ends, stopping its workload before the grantor's end
of the lease, when another node may be granted one; an agent hung rather than cut off stops nothing,
which is why the class says `lease_only`. The filesystem's failures are the workload's: a
hard-mounted export whose server is gone blocks every process reading it until it returns, and
Korpis cannot unmount it. Every client writing to the export as root can replace any entry with a
link, so the helper makes, gives away and removes what it makes only through descriptors of the
class root and of what it made, and drivers check where a volume resolves before mounting; a root
client can still swap a volume's directory for a link between that check and the mount. A shared
class trusts every client of its export with root's writes as it trusts its own nodes, and the
operator is told so. It does not mount, check an export's permissions beyond the probe above,
snapshot, or back up faster than a file-level backup of a live tree.

## Which nodes share a store

Which nodes share a store is read, not declared. At its first start on a class the helper writes a
new identifier to a file of its own name under the class directory and links it to `.korpis-store`;
`link(2)` succeeds for exactly one writer on NFS and CephFS, and a writer whose answer was lost
finds its own file's link count at two, which is how `open(2)` tells a program to take a lock on
NFS. Every other helper reads the identifier that won. Each node reports the class with that store,
and the control plane treats classes with one store as one storage. **What evaluates "these two
nodes reach the same storage":** the control plane, from what each node's helper read at its own
mount, which only the same storage could have shown both. A node whose operator copied the file
elsewhere claims a store it does not reach, and gains a workload whose volume is not found
(`volume_unavailable`).

A node's declaration is its own word, so a node of another organization declaring a store it never
mounted must not reach that store's volumes. **A store belongs to the organization of the first node
declaring it**; a node of another organization is not given it, its class kept without a store and
the refusal an effect (`node.store_refused`). A store is bound again to a declaring node's
organization once the organization holding it has no live volume in it and no node offering it, as
an effect (`node.store_bound`), so a store moved between organizations, or first declared by a node
enrolled in the wrong one, can serve its new owner. The first declaration needs the identifier,
which only the storage shows: the API shows a store only on a class it was given to and on a volume
in it. And a node's report keeps or deletes only a volume of its own workload, attached to it or
retained for it.

The control plane keeps a volume a node reports in a shared class as the store's, known by its store
and helper id whichever node reports it, its keeper the node that last reported it, where a plan on
it runs while no workload holds it.

## Block classes

One volume is one device, attached on one node at a time and formatted ext4 by the helper on its
first attach; its size is the device's, so its bytes are enforced in the write path and the class
declares `quota_enforced`. Making a device, attaching it to a node and refusing it to every other
node are each a call to the storage's own interface, which differs by vendor and product, so they
are a provider's, `block_storage` (see [extensions](extensions.md)). A block class whose
provisioning lived in the helper would put every vendor's management API and credential on every
node. The Ceph RBD and iSCSI target providers are core extensions built through that contract, as
the address routers that speak BGP are.

The control plane asks the provider to create a volume of a size and, at each attach, to attach it
to one node for one lease epoch, which the provider does by first fencing every other node (a Ceph
exclusive lock taken after blocklisting the previous holder, a SCSI-3 persistent reservation
preempting the previous key) and then answering what the node needs: the protocol, the target, and a
credential scoped to that volume where the storage can scope one. A provider that can fence declares
`storage_enforced` in its registration; one that cannot, `lease_only`. **What evaluates "the
previous node can no longer write":** the storage, told by the provider at the attach with the epoch
it is for; the agent's lease discipline remains beneath it.

A node declares a block class through its helper (`--block-class NAME=iscsi|rbd|loop:DIR`), naming
the protocol it attaches by and no store: a block class's store is `block/<organization>/<class>`,
set by the control plane when the node connects, so every node of one organization offering the
class reaches the same volumes and no node's word names another organization's. A class saying
otherwise is refused at connection.

The cost: a device is attached at the start of every placement and detached at the end, a login or
map taking seconds, and a node losing the storage loses the workload's disk as a failing local disk
would.

## Devices in the control plane

The control plane keeps a device for each volume a placed workload declares in a block class of its
node (table `block_device`), by store and the identifier its volume will have, since the device is
wanted before the volume exists; the volume the node then reports takes the identifier of the device
kept for its own workload, and a report in a block store naming none is refused. One control plane
at a time takes each device due a call and asks the `block_storage` provider the workload's
organization sees for the class: to create it; to attach it to the node holding the workload's lease
for that lease's epoch, and again whenever the lease is held elsewhere or for another epoch, as
after a failover, the provider fencing the previous holder; to detach it once no lease is held for
its node and epoch, released or past the grantor's end; and to delete it once its volume is deleted,
or once its workload is and no volume was made of it. **What evaluates "attached for this lease":**
that control plane, from the leases it stores. A workload whose devices cannot be recorded is left
to the next pass, the rest tended.

A failure is kept with its reason and asked again after ten seconds, doubling to ten minutes; the
workload's status shows each device by volume name, whether it is made and attached for the lease
held, and the provider's last failure (`korpis status` prints one line each). The credential
answered is sealed to the device by the control plane's device key (`device-1` in its data
directory) and opened only into the assignment of the node it was attached to, for that epoch, as an
ingress endpoint's key is. A device of a workload whose node falls silent stays attached there until
the grantor's end of its lease, then is detached or attached elsewhere by the provider; until a
provider answers, the device is where it was.

## Devices on the node

The helper attaches each volume's device as its provider answered, checking every part of the answer
against what its protocol allows before any reaches a command line, a sysfs write or a path: a loop
target is a bare file name in its directory, opened beneath it following no link; iSCSI logs in with
`iscsiadm`, keeping the CHAP credential in a node record deleted at logout; RBD maps through the
kernel's interface with `<user>:<base64 key>`. A device holding nothing is formatted ext4, its root
owned by the workload's root; one holding ext4 is mounted `nosuid`, `nodev` under
`/var/lib/korpis/block/<class>`, which lets the volume follow its workload; one holding anything
else is never mounted. The helper's class declares `lease_only`, which is what the node itself
guarantees.

The agent makes or mounts a block-class volume only with the device its assignment carries, waiting
until it carries one, and lets go of a device once no assignment held carries it and nothing of its
workload runs on the node, since a running object holds its filesystem. **What evaluates "nothing
holds this filesystem":** the agent, from the objects it tracks and their drivers' word; one it
cannot tell is stopped keeps the device.

**A guest is given a block-class device as its disk.** A device's kind is fixed when it is first
wanted, from whether the driver running its workload on that node takes block devices (`guest` in
the assignment). A guest's disk is never mounted, probed or read on the host; the helper formats it
only while no volume was made of it, and gives the driver a device node where a block device's image
would be, which the driver binds into the guest's jail. A device of one kind asked for a volume of
the other is refused. So a volume made for a `container` workload cannot be attached by a `microvm`
one, and a guest's disk reports no usage, which only its guest knows.

End to end, two nodes whose class attaches files through loop devices, with a provider of the tests'
own refusing an attach for an older epoch: a workload writes on the first node, that node loses
power, and the workload runs on the second, its device attached for the new epoch, holding what the
first wrote.
