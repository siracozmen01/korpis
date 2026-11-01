# Resources

What a workload reserves and what limits it, what the kernel enforces for each, and how dedicated
cores and devices are held. How quotas bound these across a tenancy tree is on [quota](quota.md).

## Contents

- [Reservation and limit](#reservation-and-limit)
- [Service classes](#service-classes)
- [Dedicated cores](#dedicated-cores)
- [I/O and processes](#io-and-processes)
- [Overcommit](#overcommit)
- [Devices](#devices)

## Reservation and limit

```
ResourceSpec
  cpu        {reservation, limit, isolation: shared | dedicated_cores}
  memory     {reservation, limit}
  disk       {reservation, limit}
  io         {limit, share}         ceilings in bytes/s and ops/s; a proportional share
  bandwidth  {egress: {reservation, limit}, ingress: {limit}}
  pids       {limit}
```

A reservation is guaranteed: subtracted from the node's allocatable and never oversubscribed. A
limit is the ceiling, and the gap between them is burst, which may be contended. Each field is what
the kernel can actually do: cgroup v2 has no I/O floor (`io.max` is a ceiling, `io.weight` a share
of contended capacity), and no node can guarantee what arrives at it, so neither carries a
reservation.

## Service classes

| Class | Condition | Sold as |
|---|---|---|
| `guaranteed` | reservation equals limit | guaranteed CPU time, dedicated RAM |
| `burstable` | reservation below limit | shared vCPU, fair share |
| `best_effort` | no reservation | free tier, batch, scavenger capacity |
| dedicated cores | `cpu.isolation: dedicated_cores` | whole physical cores, no other tenant on their SMT siblings |

Guaranteed CPU is CPU time, enforced by `cpu.max` and `cpu.weight` on cores shared with others; it
is not a dedicated core and is not sold as one. Dedicated cores are the only configuration that
defends against cross-core side channels in any tier, and their density cost is stated in the plan
that asks for them.

## Dedicated cores

**A workload asks for a whole number of physical cores**: its CPU reservation equals its limit and
is a multiple of 1,000 millicores, one core each. A core is what the kernel's topology says shares
one core's execution units (`core_cpus_list`), so on a machine with SMT on it is two logical CPUs,
and the workload gets every logical CPU of each core it holds, each counted at 1,000 millicores
against the node: four dedicated cores take 8,000 millicores there. Quota counts what is declared,
1,000 per core, since admission knows no node. An object holding cores has no CPU limit, its cores
being its bound, so wherever its CPU is shown it shows cores, never a millicore limit no kernel
holds.

**A node offers its cores**, read by the agent from the topology at start with each core's logical
CPUs and NUMA node. The core holding CPU 0 is never offered, so shared workloads, the host and the
agent always keep at least one; an operator names the offered cores with `--dedicated-cores`, and a
core with a CPU offline is not offered.

**The scheduler chooses the cores at bind and the placement holds them**: cores a placement
following its target already holds first, then cores of the NUMA node with most free (keeping cores
and memory on one node), then lowest ids. The filter keeps a node only where a driver of the tier
declares dedicated cores enforced and enough are free. Held cores are derived from placements, never
a separate ledger.

**Nothing else runs on a held core.** A driver puts a holding object in a cgroup under
`<parent>/dedicated`, its `cpuset` the cores' CPUs and their NUMA nodes' memory, and every other
object under `<parent>/shared`, whose `cpuset` the helper keeps at the node's online CPUs less those
held, writing it before a holding object is created and after one is destroyed; the kernel moves
shared tasks off a newly held core. To keep the host off too, the holding cgroup is made a remote
cpuset partition (`cpuset.cpus.partition`, kernels from about 6.7), the helper first writing the
held CPUs into `cpuset.cpus.exclusive` of each cgroup above, as the kernel requires; nothing outside
the partition, the host's agent, containerd and services included, runs on those CPUs while held.
That is the default. With `--dedicated-cores-host shared` the host keeps every CPU, and the driver's
declaration says other tenants are kept off and the host is not; without that flag, a node whose
kernel lacks remote partitions offers no dedicated cores, and preflight says why.

**What it costs and does not do.** A partition takes its CPUs from the host's own services for as
long as it is held. Per-CPU kernel threads and interrupts still run on a held core, except
interrupts the operator steers away with `irqaffinity`, which preflight names. `cpuset.mems` keeps
the workload's pages on its NUMA node, not the kernel's allocations for it. With SMT on, a workload
pays for both threads of each core. The meter records each held core for the time held. The
partition path is not run end to end on the machine this project is built on, since it would take
CPUs from that machine's other services; tested there are the opt-out (tenants kept off by the
shared `cpuset`) and the partition files written in the order the kernel asks against a tree of the
test's own.

## I/O and processes

**I/O is ceilings and shares, never a reservation.** `io.limit` sets `io.max`; `io.share` sets
`io.weight`, which does nothing unless the node runs the `io.cost` controller, so a node without it
does not offer shares and preflight says what to configure. Both are offered only where the storage
class declares them enforceable, because the cgroup cannot attribute I/O a filesystem issues from
its own threads, which on ZFS is most writes.

**`pids` is a ceiling with no reservation**, because nobody sells the right to fork, and it always
exists: a process that forks without bound exhausts the node's PID space and takes every tenant
down, the cheapest denial of service available to anyone with a shell. It is cgroup v2 `pids.max`,
4,096 processes and threads by default, applied by the driver's `Prepare` when the intent sets none,
and a driver refuses a spec without one, so it never depends on the supervisor remembering.

## Overcommit

| Dimension | Overcommit | Why |
|---|---|---|
| CPU | yes, a configurable ratio | time-shared; contention degrades gradually |
| dedicated cores | no | a pinned core is exclusive or not dedicated |
| memory | off by default, discouraged | there is no gradual degradation, there is the OOM killer |
| disk capacity | yes, thin provisioning | with the obligation below |
| disk I/O | not applicable | only ceilings and shares |
| egress bandwidth | yes, in the burst region | time-shared |
| devices, dedicated IPv4 | no | exclusive by nature |

Overcommit applies only to the burst region: allocatable for reservations is always physical
capacity, so a guaranteed workload is never oversubscribed whatever the node's ratio. A tenant told
it has 100 GB must be able to write 100 GB, so a node whose thin provisioning nears physical
exhaustion raises pressure, stops accepting placements and alerts before any tenant meets a limit
nobody told it about. Every dimension is kernel-enforced and metered or not offered, per node and
storage class, from the drivers' enforcement declarations: a dimension a node cannot enforce is not
offered there rather than displayed and quietly not held.

## Devices

A GPU is at once a resource, a placement constraint and a driver capability, so it does not fit as a
scalar. Generalized, the model also covers capture cards, licensing dongles, TPMs and precise
clocks.

**A node's devices are those the Container Device Interface describes.** CDI is the format
accelerator vendors already ship: their toolkits write files under `/etc/cdi` or `/var/run/cdi`
naming each device `vendor.com/class=name` with the edits a runtime makes to give it (device nodes,
mounts, environment, groups, hooks). NVIDIA's toolkit writes one for a machine's GPUs and their
partitions, so nothing of Korpis's knows a vendor. The agent reads the directories at start and
every half minute, as CDI's reference implementation orders them (`/etc/cdi` then `/var/run/cdi`, a
later name replacing an earlier, a name two files of one directory claim offered by neither, a file
with an unknown field not read at all), and reports each device: its CDI name; its class (from the
kind where Korpis knows it, `nvidia.com/gpu` is a GPU, otherwise as the operator maps it, an
unmapped kind listed as unclassified rather than guessed); its annotations, which selectors match;
and the host device nodes its own edits give. Two devices are the same hardware when one's nodes
contain the other's, so a GPU and any of its MIG partitions are one hardware while two partitions
are not; an aggregate such as NVIDIA's `all` is not offered, since a workload asks for a count.

| Mode | A holder has | Offered by |
|---|---|---|
| `passthrough` | the device alone while placed | a driver declaring the class and mode (the `oci` driver, character devices) |
| `timeslice` | the device shared with up to its configured shares, scheduled by the device's driver | the operator per kind, and a driver declaring it |
| `mediated` | a virtual partition from a hypervisor (vGPU) | a `vm` driver; none is built |

A hardware partition (a MIG instance) is a device of its own, given in passthrough. Timeslicing
isolates nothing between holders (memory is not partitioned, one can starve another), and the
explanation of a placement in that mode says so.

A workload asks by class, count, a selector over attributes and ids, and mode. A node passes only
where a driver of the tier declares the class and mode and enough matching devices are free: free
for passthrough when no placement holds it or its same hardware, for timeslice when fewer than its
shares hold it and none holds it in passthrough. The scheduler chooses at bind: devices already held
by a placement following its target first, then those sharing least hardware (a partition before its
whole device), then lowest ids. Placements record the devices, and what a node's devices hold is
derived from placements, so a device is freed with its placement and restored with placements. The
agent gives devices to the driver by name and the driver reads their edits from the files itself. A
device leaving the node's facts while held stays held; the agent refuses the next build naming it,
and the driver refuses it at `Create` otherwise. Each class is a quota count, a timeslice share
counting one.

**What it costs and does not do.** CDI files are the node's own configuration, written as root, and
their hooks run as root on the host when a container holding the device is created, so the agent and
the driver read only files and directories owned by root and writable by no one else, and the driver
takes a device's hooks only by reading the files itself, so a compromised agent can name a device
but never a hook. A device's RDT class and network interfaces are not applied, so a device needing
either is refused rather than given half. A workload in a user namespace opens a bound device node
as its host ids, so a node only a group can open (0660, `video`) is unusable until a udev rule opens
it. Sameness of hardware is inferred from the files, erring toward giving less, never two holders of
one. Devices are chosen request by request, greedily, so overlapping selectors can be refused where
another pairing would fit, naming the request that found too few. Devices are not metered yet. A
microVM gets no device, since Firecracker has no PCI passthrough. And no real GPU has been given to
a workload on this project's build machine, whose GPU is never passed through; tests use a stand-in
device in a CDI file of their own.
