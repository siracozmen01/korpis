# Replication

A volume on a local class may keep copies on other nodes, brought up to date on an interval, so a
workload can be started elsewhere when its node is lost. Proxmox VE's storage replication is the
same design, ZFS snapshots sent incrementally on a schedule of a minute or more, which is evidence
that the operators Korpis is for already run it.

## Contents

- [Declaring it](#declaring-it)
- [The loss is bounded and stated](#the-loss-is-bounded-and-stated)
- [Choosing replicas](#choosing-replicas)
- [Transfers](#transfers)
- [On the node](#on-the-node)
- [Failover onto a replica](#failover-onto-a-replica)
- [What it costs](#what-it-costs)

## Declaring it

A volume declares `replicas`, the number of other nodes keeping a copy, and `replication_interval`,
one minute at least and five by default. It is accepted only where some node serving the
organization offers its class able to send increments (btrfs today, ZFS once built), which placement
then requires of the node it binds; a copy sent whole every interval is not one that can be kept
current, so elsewhere it is refused. Replicas on an attached volume, none, or an interval under a
minute are refused. The intent's `continuity.replica_failover` (`manual` or `automatic`) says what
happens when the node is lost.

## The loss is bounded and stated

A replica is as old as the last increment it received, and the volume's status shows each replica's
age. A workload placed again on a replica's node loses what was written after that increment, at
most one interval and a transfer. So failover onto a replica is automatic only for a workload
declaring it accepts that loss; for every other, a replica is what an operator's `evacuate` starts
from, its plan naming the age it starts from. A `keep_running` workload is never failed over, so a
derived `on_lease_expiry` is `stop` for a replicated volume whose workload declares automatic
failover, and declaring `keep_running` beside it is refused.

## Choosing replicas

Every heartbeat interval a pass tends each volume on one node's disk that a workload holds or that
keeps replicas, in a transaction of its own under one lock. It chooses as many nodes as declared
among those serving the organization that are ready and offer the class able to send increments with
room in the class and in the disk the node allocates, other failure domains first (the labels the
organization's placement policy spreads by, then `region`), then the most room. It keeps them from
one pass to the next; drops a replica whose node no longer serves the organization or offers such a
class, and every replica of a volume no declaration asks for; marks stale one ten intervals without
receiving; and records each change as an effect (`volume.replica_chosen`, `volume.replica_dropped`,
`volume.replica_stale`). A replica's bytes are in its node's allocation and the project's storage
quota as the volume's own are, since they take as much space, and nothing else limits them.

The same pass makes a transfer due to each reachable replica with none under way that holds nothing
or holds what is an interval old: one snapshot, named by an identifier the control plane chooses. A
transfer not received within the hour its ticket lasts is given up and made due again.

## Transfers

**Over the mesh and nothing else.** The receiving agent listens on its own mesh address, on TCP port
51820, and the sending agent connects from its own. The control plane gives the sending node, for
each transfer, a ticket signed with its token key over a purpose label of its own (`korpistt_`), so
nothing else the key signs is taken for one: it names the volume, both nodes, the snapshot sent and
the one it is an increment from, and an expiry. The receiver admits a stream only under a ticket its
token keys open that names this node, a replica it was told it receives, the snapshot it was told
comes, and the node it was told sends it, from that node's mesh address, which the kernel's
WireGuard admits from no key but that node's. Its table admits that address to the port from the
mesh alone, under the comment `transfer`. **What evaluates "this stream may be written into this
replica, from there":** the receiving agent, from the ticket and its token keys, and its kernel,
from the mesh's keys; neither asks the control plane, so a transfer survives the control plane's
absence until its ticket expires. [Migration](migration.md)'s copies cross the same way.

A node is told, in a `Replicas` message when its session opens and whenever it changes, each
transfer from its volumes with its ticket and the snapshots it keeps, and each replica it keeps with
the snapshot it holds, the one being sent, and its sender. What it is told is the whole of what it
keeps, so it releases the rest; a node told nothing yet releases nothing. The session keeps what it
is told on disk before acting, so a transfer outlives an agent restart. A sending node retries a
failed transfer every ten seconds while its ticket lasts. The receiving node journals how each
receive ended, and the control plane believes it only of the transfer under way to that node of the
snapshot it names. **What evaluates "this replica received this snapshot":** the control plane, from
the transfer it made due and that node's journal; a report from any other node, or of another
snapshot, is refused (`journal.rejected`).

## On the node

The helper keeps what replication holds under the class root's `.replicas/<volume>/`: on the
volume's node the read-only snapshots it sent, on a replica's node those it received. Sending is the
kernel's `BTRFS_IOC_SEND`, whole or from the snapshot both sides hold, into a pipe whose read end
the agent is given. Receiving is btrfs-progs' `btrfs receive`, confined by its own `chroot` to the
replica directory, reading a pipe the agent writes, rather than a stream parser of Korpis's own:
btrfs-progs is the format's reference, and a parser written again would be the one place the
format's rules could be read differently. So a node holding replicas needs btrfs-progs. A snapshot
is held once `btrfs receive` made it read-only, which it does at the stream's end alone; anything
else of a receive ended short is removed. After each interval the older snapshot is released on both
sides.

## Failover onto a replica

A workload whose node the sweep marked unreachable and whose declaration says automatic failover
reaches each volume on that node at the node keeping its freshest replica, so all such volumes'
freshest replicas must be on one node for it to be placed (see [placement](placement.md)). Placed
there, in one transaction:

- the volume record moves to that node under a helper id of its own, never the one it had, which a
  copy left by an earlier failover may still hold;
- the copy it leaves becomes a volume of its own on the old node, retained and held by no workload,
  recording the volume it diverged from, when, and when the replica received what the volume now
  holds; it is listed on that volume as divergent, never collected, and deleted only by a plan,
  since it holds what the failover lost;
- the volume's snapshots, which are where the copy is, become the copy's;
- the replica it is made from is no longer one, its snapshot kept until the node reports the volume
  made, and every other replica is marked stale and sent whole from the new node.

The remake plan's `promote_replica` step makes the volume a writable snapshot of the replica's, the
helper answering the copy it already made when asked again, which it tells by the subvolume's parent
UUID. The failover's effect records `lost_since`, when the stalest replica a volume was made from
received. A divergent copy is deleted as any retained volume, by a plan a person approves, executed
by its node through a residual assignment once heard again; until then nothing writes or removes it,
and a workload may attach it to read what it holds.

## What it costs

Each interval a snapshot is held on the volume's node until every replica has the next, so an
unreachable replica holds space there; past ten intervals it is stale, its snapshot released, and it
is sent whole on return. Transfers share the mesh and are not shaped. Replication is not
synchronous, which would put the network into every write's latency, and a volume in a shared or
block class is not replicated: its storage keeps its own copies.
