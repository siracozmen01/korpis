# Runtimes

How a workload runs: the isolation tiers an operator chooses between, the drivers that implement
them, and the interface between a node and its drivers. How a `microvm` guest is built and reached
is on [guests](guests.md); probes on [health](health.md); what a driver must pass to conform on
[driver conformance](driver-conformance.md).

## Contents

- [Isolation tiers](#isolation-tiers)
- [Drivers](#drivers)
- [Capabilities](#capabilities)
- [Lifecycle](#lifecycle)
- [What a driver applies](#what-a-driver-applies)
- [What the interface never assumes](#what-the-interface-never-assumes)
- [Ids on disk](#ids-on-disk)
- [Content](#content)
- [What is not in the interface](#what-is-not-in-the-interface)

## Isolation tiers

Isolation strength is an operator's choice, not a property of the platform. Products in this market
froze it: containers mandatory (Pterodactyl and its forks), which puts Windows binaries, several
anti-cheat systems and GPU workloads out of reach; no containers (PufferPanel, GameAP), which lets
an exploited game server reach the rest of the machine; or containers as a deployment toggle (AMP)
rather than a graded model. The interface expresses a Windows Job Object and a Firecracker microVM
in its first version, beside a containerd container, since retrofitting an abstraction rewrites
every consumer of it.

| Tier | Boundary | Mechanism | Boot | Overhead | Untrusted code |
|---|---|---|---|---|---|
| `process` | shared kernel | cgroups v2, namespaces, seccomp, Landlock, no image | instant | none | no |
| `container` | shared kernel | OCI image, every namespace, user namespace, seccomp, read-only root | ~100 ms | minimal | with care |
| `microvm` | hypervisor | own kernel, minimal device model | ~150 ms | ~30 MB | yes |
| `vm` | hypervisor | own kernel, full device model, own init | seconds | high | yes |

The tier is what an operator reasons about: code I did not write is `microvm`, my own service is
`container`, a trusted helper needing bare performance is `process`, a customer's VPS is `vm`.
Drivers are an implementation concern beneath that sentence.

**There is no unhardened container tier.** `container` always means a user namespace, a seccomp
profile, `no-new-privileges`, a read-only root and dropped capabilities. An operator who wants
weaker confinement selects `process` knowingly; making the weak option a deliberate downgrade rather
than an omission is the difference between a default and a trap.

**What the `oci` driver makes of it.** The process runs in its own user, mount, PID, IPC, UTS,
cgroup and network namespaces, mapped onto the workload's id range, with `no-new-privileges`, a
read-only root, and `/tmp`, `/run` and `/dev/shm` as small tmpfs mounts so a read-only root is
livable. It keeps nine capabilities, all namespaced (`CHOWN`, `DAC_OVERRIDE`, `FOWNER`, `FSETID`,
`KILL`, `NET_BIND_SERVICE`, `SETGID`, `SETUID`, `SETPCAP`), never `SYS_ADMIN`, `NET_ADMIN`,
`NET_RAW`, `MKNOD` or `SYS_PTRACE`. Its seccomp profile is an allowlist narrower than containerd's
default: no `io_uring`, `ptrace`, keyrings or mounting, and no creating or joining a namespace,
which inside a user namespace would otherwise be open. The kernel's sensitive `/proc` and `/sys`
entries are masked or read-only.

On a node that gives workloads a network, the user and network namespaces are made by the helper and
joined by the container, the driver still giving runc the mapping, which runc checks against the
namespace it joins. The network namespace must be owned by the container's user namespace, because
the kernel lets a container mount sysfs only in a network namespace it owns. The workload's identity
directory is the one mount not idmapped: bound read-only, `nosuid`, `nodev`, `noexec` at
`/run/korpis`, owned by the supervisor and reached through the socket's mode, so the workload can
connect and cannot replace the socket. An image whose user is a name rather than a number is
refused, since resolving it means reading the image's own `/etc/passwd`.

## Drivers

A driver implements one or more tiers on one operating system.

| Driver | Tiers | OS | Built |
|---|---|---|---|
| `oci` | `container` | Linux | yes: containerd and runc, the first driver |
| `firecracker` | `microvm` | Linux | yes: the second, the most different one |
| `native` | `process` | Linux | no: Landlock, cgroups v2 and `unshare`, the design Nomad's `exec2` shows |
| `qemu` | `vm`, `microvm` | Linux | no: full virtualization, VFIO passthrough |
| `winjob` | `process` | Windows | no: Job Objects and restricted tokens |
| `winsilo` | `container` | Windows | no: Server Silos through the Host Compute Service |

The `oci` driver ships excellent before anything else ships adequate, and the second driver is the
most different one rather than the easiest, because a second implementation is the test of an
interface (see [what the second driver
changed](driver-conformance.md#what-the-second-driver-changed)).

**Each driver is a process of its own**, reached by the agent over a local socket. In one process
the agent would hold the union of every driver's privileges, which is close to root: `qemu` needs
KVM and devices, `oci` needs containerd, `native` almost nothing. A driver crash then takes down
neither the agent nor the workloads it supervises, a third party ships a driver without being
merged, and a driver upgrades independently.

**The agent speaks to all of a node's drivers as one runtime.** It asks each its name and
capabilities at start, and reports every one on the node, which [placement](placement.md) filters
by. A `Prepare` goes to the driver the intent's `runtime.driver` names, and is a rejection naming
the missing driver where the node has none; the prepared spec names its driver, and `Create` goes
there. Every other verb goes to the driver that made the object, from a table filled by `Create` and
`Recover`. An identifier is never marked with its driver: the meter keys usage by identifier, so one
that changed when a second driver was added would read as a new object and be counted twice. The
cost: two drivers issuing the same identifier cannot both be routed, the first keeps it and the
second's object is left out of adoption and said. Two drivers declaring one name are refused at
start.

## Capabilities

The control plane asks and never assumes, as Nomad's task driver interface does. A version number is
not a capability statement: two builds of one driver on different kernels differ, and inferring from
a version is how a scheduler places a workload on a node that cannot run it.

```
Capabilities
  tiers, console (none | logs | interactive | serial), exec, signals (names)
  hot_resize, hot_volume_attach, live_migrate
  runtime_snapshot     null, or where a snapshot restores: CPU model or template, hypervisor
                       version, host kernel range
  devices, device_modes   classes, and passthrough | timeslice | mediated for every one
  rootfs_sources       oci_image | directory | disk_image | iso
  network_modes        veth | tap | host | none
  enforcement          per resource: enforced, mechanism, granularity
  volume_semantics     filesystem | block_device
  file_access          host_worker | guest_worker | none
  id_mapping           how workload ids reach the disk
  config_schema        the driver's own configuration, as JSON Schema
  disruption           per tier, the step actions it makes disruptive
  identity_dir, guest_agent, guest_filesystems, cgroup_parent
```

**`enforcement`, because a limit not held is not displayed.** Windows has no cgroups: job objects
bound memory and CPU differently, and NTFS quota is per user, not per directory tree. A Windows
driver declares disk quota not enforced, the scheduler refuses a workload asking for a limit its
driver cannot hold, and the interface does not display it. Network shaping is held by the node, not
the driver: the agent adds `endpoint_accounting` wherever its helper gives workloads networks,
`ingress` and `endpoint_shaping` wherever the helper can write qdiscs on the workload's link, and
`egress` only where an uplink is declared and shaped, since a reservation is held there.
`dedicated_cores` is declared enforced only where the driver keeps holding objects in a cpuset of
their own and the rest in the shared one ([resources](resources.md#dedicated-cores)); its
granularity says whether the host is kept off too.

**`volume_semantics`, because a VM's volume is a block device.** Under `process` and `container` a
volume is a directory on a filesystem the host controls, its quota fails a write with `EDQUOT`, and
inodes can run out. Under `microvm` and `vm` it is an image the guest owns: the bound is the device
size, the guest sees `ENOSPC`, and the host knows nothing of its inodes. The difference reaches the
user rather than being smoothed over; [storage](storage.md) carries what each means.

**`devices`, per tier, because Firecracker has no PCI passthrough.** Without devices in the
declaration the scheduler would accept `microvm` with a GPU and fail at apply time.

**`disruption` is the driver's layer** of the [classification
chain](plans.md#where-disruptive-comes-from): a stop on a `vm` costs a boot, on a `process` it does
not. A name outside the plan vocabulary is refused, since an ignored entry would make a step less
disruptive than declared. The control plane takes the union over every node serving the organization
and offering the tier, because the layer only adds; the cost is that one driver declaring an action
disruptive makes it so for every workload of that tier its organization could run. The `oci` driver
declares nothing.

**`config_schema` is the honest part of the abstraction.** A VM needs a kernel, a bootloader and
cloud-init; a container needs none. The lifecycle is common and the configuration driver-declared;
Korpis validates against the schema without understanding it.

## Lifecycle

```
Prepare(Intent)       -> PreparedSpec | Rejection    pure: validates, resolves, changes nothing
Create(PreparedSpec)  -> RuntimeID
Start(restore_from?), Stop(signal, grace), Kill, Destroy
Checkpoint(into)      -> bytes                         for a driver declaring runtime_snapshot
Recover()             -> []RuntimeID                 adoption after an agent restart
Inspect -> RuntimeStatus; Events; Stats -> usage and counters
Signal(name); Exec(command, timeout, limit) -> Stream; PingGuest; console attach, input, resize
Files; StartInstall, InstallStatus, ForgetInstall      for guests (guests.md)
```

- **`Prepare` is pure.** It validates that this driver on this node can satisfy the intent and
  returns a prepared spec or a rejection with a reason. This lets a plan be known satisfiable before
  anyone approves it and makes dry-run free; a `Prepare` with side effects would make every dry-run
  a partial apply. The `oci` driver's reads only its arguments: the intent and the recipe document
  the assignment carries, from which it takes the image for this architecture pinned by digest, the
  entry, and the variables and arguments the configuration resolves to.
- **`Create` and `Start` are separate.** Resources are allocated, volumes mounted and the network
  attached before anything executes, so failures happen where nothing runs yet; Windows needs it,
  creating a job object and starting a process in it being distinct operations.
- **`Checkpoint` pauses, writes and ends; `Start` from its directory resumes.** A driver declaring
  `runtime_snapshot` writes a running object's execution state into a directory of the workload's
  own state volume, `<workload>.live_state` directly beneath a volume root, and ends it; a start
  naming that directory resumes it. The state records the workload it was taken of and where it
  was taken (hypervisor version, CPU template, host kernel `major.minor`), and a driver resumes only
  its own object's state taken where it restores here: any other is refused, never booted in its
  place, since a workload that was running is not silently started afresh. The `firecracker`
  driver pauses the guest and has its Firecracker write a full snapshot into its chroot, copies it
  and the boot, image and config drives the guest holds open into the directory, each opened
  beneath it through no link and written anew, since the destination builds its own drives apart
  and their bytes differ; a resume copies them back into the object's directory and has a
  Firecracker started with no configuration load them. Its snapshots restore by the same
  Firecracker version on the same host kernel line alone, as Firecracker documents
  (`docs/snapshotting/snapshot-support.md`), so it declares that kernel as both ends of its range.
- **`Recover` is not optional.** It is [adoption](node-agent.md): a driver that cannot enumerate and
  re-adopt what it made turns every agent restart into an outage.
- **Every verb is idempotent, and an unknown object is `NotFound`.** A verb asked of an object
  already where it asks succeeds and changes nothing; `Create` of a spec already made answers the
  same object. Every verb naming an object the driver does not hold answers `NotFound`, `Destroy`
  included, and the node takes `NotFound` from `Stop`, `Kill` and `Destroy` as done. Answering
  success instead would lose the difference between an object removed and one never there. So a
  driver removes the record that says it holds an object last.
- **Events only make the supervisor look sooner.** For every start, exit, pause, resume, delete or
  out-of-memory of a task, the event carries the status read back with `Inspect`, never built from
  the event; one lost in a reconnection costs latency and nothing else, since the reconciler reads
  every object on its period anyway.
- **`Inspect` says when an object last started and ended.** An agent counts an adopted run's timeout
  from `started_at`; without it, from the adoption, running the run longer by however long the agent
  was away. containerd keeps when a task exited but not when it started, so the `oci` driver records
  each start in the label `korpis.started_at` just before starting the task, which can name an
  instant a few milliseconds early.
- **An object its kernel ended for want of memory says `oom_killed`**, its exit 137, which tells a
  limit enforced from a crash. The `oci` driver reads it from the cgroup's `memory.events`, which
  survives a driver restart; a guest compares the kernel's count in `/proc/vmstat` from the entry's
  start to its end. Neither says which process the kernel chose.
- **`Stats` answers what metering sums**: the usage now, and counters since the object was made (CPU
  time and time throttled, bytes read and written, memory and its high-water mark), each absent
  where unreadable, so a controller not enabled is a quantity not measured rather than a zero.
  Keeping a workload's counters continuous across its objects is the agent's meter, not the
  driver's.
- **Signals are names.** A driver delivers what it can and declares exactly that; one declaring none
  tells a workload only its graceful default and its kill, in that order.
- **`Exec`** runs a command in a running object's namespaces and cgroup, as its own process runs:
  its user, environment, working directory, capabilities and limits. It streams stdout and stderr
  together, at most the caller's limit and never over 64 KiB, then the status (128 plus the signal
  where one ended it) and whether it was killed for outliving its timeout, at most ten minutes, or
  had output dropped. Output past the limit is read and dropped, so the command never blocks on a
  full pipe. The command is not validated against the intent, which the driver does not see then: a
  supervisor asking another command could run it, which is no more than it can do with the objects
  it creates. There is no input to a command yet.

**A container whose shim died is started again.** When a shim dies, containerd forgets the task and
runs the shim's `delete` in its bundle; containerd on this project's build machine killed that
cleanup after five seconds every time and left the bundle, and every later task was refused as
existing until containerd restarted. So the `oci` driver, creating a task on a container containerd
knows no task of, asks again for up to twenty seconds while containerd's cleanup holds the place,
then finishes the cleanup itself in that one container's bundle, which its `--containerd-state`
names.

## What a driver applies

**A device reaches a driver by name, and its edits from the node's own files.** Before `Create` the
agent fills in the spec's devices from what the placement holds (id, class, mode) and nothing of how
each is given. The driver reads the node's CDI files itself and applies the edits they give. The
driver's checks are what stand between a compromised supervisor and root, and an edit names host
paths to mount and programs to run as root, so a supervisor never passes an edit. The `oci` driver
binds each device node from the host node the edit names, since a container in a user namespace
cannot make one and runc binds from the container path, with a cgroup rule for that node's type and
numbers; binds the edit's mounts; appends its environment and groups; and places each hook at its
OCI hook point. It refuses a node that is not a character device on the host, numbers not the host
node's, a node's mode or owner, and Intel RDT or network-device edits. The `firecracker` driver
declares no device. See [resources](resources.md#devices).

**Dedicated cores reach a driver as a cpuset.** Before `Create` the agent fills in the cores the
placement holds, each with its logical CPUs and NUMA node. A driver checks them against the topology
it reads itself and refuses a core the machine lacks, a CPU not that core's, and the core of CPU 0.
It puts the object in a cgroup of its own under its parent's `dedicated` cgroup, a partition unless
its operator chose otherwise, with no CPU quota or weight: a quota of the declared millicores would
throttle it below its threads. The `oci` driver writes the container cgroup's
`cpuset.cpus.exclusive` and `cpuset.cpus.partition` after the task is created and before it runs,
failing the start with the kernel's reason where no partition is made. The `firecracker` driver has
the jailer write them, gives the guest one vCPU per logical CPU, and pins each vCPU thread. Before a
holding object runs, either driver reads back that its parent's `shared` cgroup is off its CPUs. An
adopted object's cores come from what its driver reports it was made holding, never from its
assignment, and an object is built again when its placement's cores change, which is a restart.

**Consoles.** Both drivers declare `none`, `logs` and `interactive`. `Prepare` returns the stream's
limits and the secrets it redacts, and the agent fills in its directory, a volume the helper makes.
The `oci` driver writes a task's output through containerd's logging binary, `korpis-stream-writer`,
which outlives the driver as the task does; an interactive console runs the task under a terminal
with a stdin FIFO the writer holds open. Input and resizes go to the writer on a second FIFO,
recorded with who sent them before reaching the workload; input finding the writer gone is refused
or recorded as dropped, never left to stall the call. A driver refuses a stream directory not the
workload's own, and a container with a stream when started without a writer, since it would lose the
console silently. See [streams](streams.md) and, for a guest's serial console,
[guests](guests.md#consoles).

## What the interface never assumes

These assumptions are invisible until they are load-bearing, and each would close a tier.

| Never assume | Instead | Closes if violated |
|---|---|---|
| POSIX signal numbers | driver-declared signal names | Windows |
| uid and gid identity | an opaque security context the driver interprets | Windows |
| cgroups | resources declared semantically; the driver maps them | Windows, VMs |
| a PID identifies the workload | an opaque driver-issued `RuntimeID` | VMs, Windows |
| one filesystem tree the agent can assemble | mounts declared as specs the driver realizes | VMs, microVMs |
| the workload is a process the agent's kernel sees | liveness from `Inspect`, never `kill(pid, 0)` | VMs, microVMs |
| a command line starts the workload | the rootfs source declares its entry | VMs, ISOs |
| the agent shares a filesystem with the workload | file access through the workload's own worker, on the host or in a guest | microVMs, VMs, Windows |
| a uid on disk means the same on every node | workload-relative ids, mapped at mount | migration |
| stdout and stderr are the output | console mode is declared; serial and VNC are modes | VMs |
| the node sets a workload's addresses from outside | the driver is given them, and a guest sets its own | microVMs, VMs |

The file-access and output rows fail quietly: an agent reading a workload's files by a host path
works for containers and is impossible for a microVM, and by the time anyone notices, the assumption
has spread through the file manager, backups and the recipe installer. Both drivers are Linux, so
the rows marked Windows are held by no implementation yet and stay claims until a Windows driver is
built.

## Ids on disk

- **Ranges are allocated by the control plane, per workload, unique on its node**, recorded in the
  assignment and given back at retirement. A range has to be distinct from every other on the same
  kernel, which is isolation; distinct across machines buys nothing, and would cap an installation:
  ranges of 65,536 ids in a 32-bit space leave at most 49,151 above the host's own, a limit by
  scale. The control plane allocates under the per-node lock it takes to seal a node's set, the
  lowest free slot from 2^30 upward, above a host's users and its rootless users' subordinate
  ranges. A range is reused only once its workload is retired, since an orphaned object still runs
  under it until then.
- **Files on disk carry workload-relative ids** (0 to 65,535), applied at mount with idmapped
  mounts, so ownership on disk does not depend on the host range and migration copies bytes with
  ownership intact. The driver declares the mechanism (`id_mapping`): idmapped mounts where the
  filesystem supports them (ext4, XFS, btrfs, ZFS from OpenZFS 2.2), and a slower fallback shifting
  ownership at copy time where not. A storage class supporting neither cannot host user-namespaced
  workloads that migrate, and the scheduler knows it.

## Content

The agent owns the node's content store, not the drivers. Recipes, images and disk images are
fetched once per node, verified against their digest, and shared by every workload referencing them,
where Pterodactyl downloads the same bytes again for every server. Drivers reference content by
digest and never fetch: one fetch and one signature check per digest per node; a compromised driver
has no registry credentials or network path to a registry; and the control plane can hint a digest
to a node before placing there, so start latency is not a download. A `container` and a `microvm`
from one base share fetched layer blobs, not realized storage: an unpacked overlay and a guest image
share no blocks.

**How the store is kept.** Content is fetched by digest only; a tag is a question the registry may
answer differently tomorrow. An index is resolved on the node to its own platform's manifest. Every
blob is written to an ingest file that refuses bytes past its declared size and is hashed as
written, synced, then renamed to its digest's name, so a file in the store is whole and is what its
name says; a name is checked to be a well-formed digest before it becomes a path. A driver reads the
store and never writes it.

Content is collected by reference count plus a grace period: a workload restarting on a new recipe
version drops the old one to zero, and deleting it at once would mean downloading it again on the
next rollback. Where the class supports reflinks, a workload's writable layer is a copy-on-write
clone of the shared base.

**Install content.** The store also keeps a recipe's files, fetched by the recipe artifact's digest,
and what an install's `fetch` names: downloaded once per node, hashed as written, kept under the
`sha256` or `sha512` the recipe declared only if it matches. The hash is what is trusted, so `http`
is allowed; redirects are followed at most ten times, no proxy is taken from the environment, and a
fetch sending more than 256 GiB is stopped. What a fetch downloads is kept apart, in `fetched/`, and
a `fetch` reads nothing else: the store also holds recipe files and layers pulled from registries
that may be a tenant's private one, and a recipe naming the digest of another tenant's private file
would otherwise have had it copied into its own volume, knowing a hash being access. Bytes one
install fetched from a public URL do serve another that names their hash without knowing the URL,
which is the deduplication intended, the bytes having been public; a file that must stay private
ships in a private recipe. A hostile URL can still make a node download up to the bound before the
hash refuses it.

The content store and a backup repository both address by content and are deliberately separate:
different trust models (public and private artifacts by digest, against tenant-private encrypted
data), lifetimes and failure consequences. Merging them would put tenant data and public images
under one garbage collector.

**How the `oci` driver hands content to containerd.** It writes each blob into containerd's content
store, commits the manifest with the garbage-collection labels tying its configuration and layers to
it, and names the image. It unpacks each layer with containerd's diff service into a snapshot named
by its chain identifier, so images sharing a base share snapshots, and refuses a layer not unpacking
to the digest the configuration names. All of it happens under a containerd lease taken for the
create, so nothing half-assembled is collected or left behind. The container's snapshot is prepared
remapped onto the workload's range (containerd's `uidmapping` and `gidmapping` labels), which lets a
read-only image owned by host root be the root of an unprivileged process. Every object lives in
containerd's `korpis` namespace.

## What is not in the interface

A driver owns the execution boundary and nothing else; whatever else is pushed into it must be built
once per driver and will behave a different way each time.

- **Live migration** is an orchestration of primitives (`runtime_snapshot`, volume snapshots,
  content staging, lease transfer) sequenced by the agent and control plane; as one driver call,
  each driver would reimplement the resumable, verifiable, reversible state machine differently. See
  [migration](migration.md).
- **Networking**: drivers declare modes and attach to an interface the node made. See
  [addresses](addresses.md).
- **Backup** follows from the storage class, not the runtime. See [backup](backup.md).
- **Resizing a running object** has no verb: a change of resources recreates the object, and no
  driver declares `hot_resize`. Adding it needs what a resize answers when the kernel cannot take it
  at once (memory below what is in use), how a plan calls an in-place resize non-disruptive, and a
  microVM's side, whose memory and processors are fixed when Firecracker starts.
