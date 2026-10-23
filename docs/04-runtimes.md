# Runtime Drivers and Isolation Tiers

**Status:** design **Date:** 2026-08-07 **Depends on:** [`01-model.md`](./01-model.md),
[`02-architecture.md`](./02-architecture.md) **Implements:** Bet 2, Rules K-9, K-10, K-18

---

## 1. The claim being tested

Bet 2 says isolation strength is an operator choice, not a platform property. Every product in this
market froze that decision:

- Pterodactyl and every fork: containers, mandatory. Windows binaries, several anti-cheat systems,
  GPU workloads, and custom kernel modules are permanently unreachable.
- PufferPanel and GameAP: no containers. An exploited game server can reach other services on the
  machine.
- AMP: containers optional, but as a deployment toggle rather than a graded model.

The bet only pays if the interface can express a Windows Job Object and a Firecracker microVM on
the same day it expresses a containerd container (not eventually, but in the first version) since
retrofitting an abstraction means rewriting every consumer of it.

This document is the interface. If it leaks, Bet 2 fails.

---

## 2. Isolation tiers

Four tiers, ordered by the strength of the boundary. The operator chooses one per workload.

| Tier | Boundary | Mechanism | Boot | Overhead | Untrusted code |
|---|---|---|---|---|---|
| `process` | shared kernel | cgroups v2, namespaces, seccomp, Landlock, `unshare`, no image | instant | none | **no** |
| `container` | shared kernel | OCI image, full namespaces, user namespace remapping, seccomp, read-only rootfs | ~100 ms | minimal | with care |
| `microvm` | hypervisor | own kernel, minimal device model | ~150 ms | ~30 MB | **yes** |
| `vm` | hypervisor | own kernel, full device model, own init and lifecycle | seconds | high | **yes** |

**There is no unhardened container tier.** `container` means user namespace remapping, a seccomp
profile, `no-new-privileges`, a read-only root filesystem, and dropped capabilities, always, not as
an option someone can forget to enable. An operator who wants weaker confinement selects `process`
explicitly and knowingly. Making the weak option require a deliberate downgrade rather than an
omission is the difference between a default and a trap (P4).

**What the `oci` driver makes of it** (2026-10-13, from the implementation, and checked against
runc on a 6.8 kernel). The process runs in its own user, mount, PID, IPC, UTS, cgroup, and network
namespaces, mapped onto the workload's id range (§4.5), with `no-new-privileges`, a read-only root,
and `/tmp`, `/run`, and `/dev/shm` as small tmpfs mounts so a read-only root is livable. It keeps
nine capabilities, all namespaced: `CHOWN`, `DAC_OVERRIDE`, `FOWNER`, `FSETID`, `KILL`,
`NET_BIND_SERVICE`, `SETGID`, `SETUID`, `SETPCAP`; never `SYS_ADMIN`, `NET_ADMIN`, `NET_RAW`,
`MKNOD`, or `SYS_PTRACE`. The seccomp profile is an allowlist, narrower than containerd's default:
no `io_uring`, no `ptrace`, no keyrings, no mounting, and no creating or joining a namespace, which
inside a user namespace would otherwise be open to the workload. The kernel's sensitive `/proc` and
`/sys` entries are masked or read-only. Three limits are honest rather than hidden: until the
networking of Phase 1 the network namespace held only loopback, so a spec that published a port was
refused rather than accepted and left unreachable (since that networking was built, a container
joins the namespace the helper made for its workload, with its routed link and its endpoints' ports
forwarded by the node, §4.1 of `07-networking.md`, and has only loopback when it is given no
network); an image whose user is a name rather than a number is refused, because resolving it means
reading the image's own `/etc/passwd`, which the driver does not do; and a volume is bind-mounted
with an idmapped mount, so its files keep workload-relative ids on disk. The workload's identity
directory is the one mount that is not idmapped: it is bound read-only, `nosuid`, `nodev`, and
`noexec` at `/run/korpis`, keeps the supervisor's ownership, and is reached through the socket's
mode, so the workload can connect and cannot replace or remove the socket (§6 of
`02-architecture.md`).

> Added 2026-10-15, from the implementation: on a node that gives workloads a network (§4.1 of
> `07-networking.md`), the user and network namespaces are not made by the runtime but joined:
> the helper makes the pair, the user namespace mapped onto the same id range, and the driver
> joins both by path while still giving runc the mapping, which runc checks against the
> namespace it joins. The network namespace has to be owned by the container's user namespace,
> because the kernel lets the container mount its sysfs only in a network namespace it owns.

**The tier is what an operator reasons about.** "This tenant runs code I did not write" →
`microvm`. "This is my own service" → `container`. "This is a trusted helper and I need bare
performance" → `process`. "The customer bought a VPS" → `vm`. That sentence is the entire
user-facing model; drivers are an implementation concern below it.

---

## 3. Drivers

A driver implements one or more tiers on one operating system.

| Driver | Tiers | OS | Notes |
|---|---|---|---|
| `oci` | `container` | Linux | containerd + runc. **The day-one driver** (Rule K-18) |
| `native` | `process` | Linux | Landlock LSM + cgroups v2 + `unshare`, no container runtime dependency, the design Nomad's `exec2` demonstrates |
| `firecracker` | `microvm` | Linux | untrusted code, fast boot, minimal attack surface |
| `qemu` | `vm`, `microvm` | Linux | full virtualization; the `machine` workloads of `01-model.md` |
| `winjob` | `process` | Windows | Job Objects + restricted tokens |
| `winsilo` | `container` | Windows | Server Silos via the Host Compute Service |

Rule K-18 is binding: `oci` ships excellent before anything else ships adequate, and the second
driver is the most different one rather than the easiest (rule D in §2 of `20-roadmap.md`). The
interface must accommodate all six from the first version; the implementations arrive in order of
evidence.

> Corrected 2026-10-12 (second external review, S60). This paragraph supported K-18 with a claim
> about VirtFusion's market history that no document cited. K-18 is a design decision, and §4 of
> `research/evidence.md` now says so and states its basis.

### Drivers are separate processes

Each driver runs as its own process, connected to the agent supervisor over a local socket.

This is not modularity for its own sake. It follows directly from §6 of `02-architecture.md`: no
component holds more privilege than its job requires. The `qemu` driver needs device access and
KVM; the `oci` driver needs containerd; the `native` driver needs almost nothing. In one process
the agent would hold the **union** of every driver's privileges, and the union is close to root.

Three further properties come free: a driver crash does not take down the agent or the workloads it
supervises; a third party can ship a driver without being merged into Korpis; and a driver can be
upgraded independently of the agent.

**A node runs one process per driver, and the agent speaks to all of them as one runtime**
(2026-10-23, as the second driver was begun). Each is asked its name and capabilities when the
agent starts, and every one is reported on the node, which is what placement filters by (§2.1 of
`05-scheduling.md`). A `Prepare` goes to the driver the intent's `runtime.driver` names, and is a
rejection naming the missing driver where the node has none of that name; the prepared spec names
the driver that prepared it, and its `Create` goes there. Every other verb goes to the driver that
made the object, which the agent keeps in a table filled by `Create` and `Recover`, and asks each
driver in turn for an identifier the table lacks. It never marks an identifier with its driver:
each driver issues its own, and the agent's meter keys what it has read of each object by its
identifier (§2 of `15-observability.md`), so an identifier that changed when a second driver was
added would read as a new object and count its usage twice. What it costs: two drivers that
issue the same identifier cannot both be routed, so the first to report it keeps it and the
second's object is left out of adoption and said; Korpis's own drivers issue identifiers that
cannot meet, and a third party's driver is asked to do the same. Two drivers declaring one name
are refused when the agent starts, since an intent names its driver.

---

## 4. The interface

### 4.1 Capabilities are declared, never inferred

```
Capabilities
  tiers                []Tier
  console              []ConsoleMode      none | logs | interactive | serial
  exec                 bool               run a command inside a running workload
  signals              []SignalName       driver-defined names, not POSIX numbers
  hot_resize           []ResourceKind     which resources change without a restart
  hot_volume_attach    bool
  runtime_snapshot     SnapshotCaps?      capture execution state, not just disk; null if not
                                          supported, else where a snapshot can be restored
  live_migrate         bool
  gpu                  []GPUMode          none | passthrough | mediated | timeslice
  devices              []DeviceClass      gpu | usb | pci | tpm | serial, per tier
  rootfs_sources       []SourceKind       oci_image | directory | disk_image | iso
  network_modes        []NetworkMode      veth | tap | host | none
  enforcement          map[ResourceKind]EnforcementDecl
  volume_semantics     VolumeSemantics    filesystem | block_device
  file_access          host_worker | guest_worker | none     (§4.4)
  id_mapping           IdMapCaps          how workload uids reach the disk (§4.5)
  config_schema        JSONSchema         driver-specific configuration
  disruption           map[Tier][]StepAction   what it makes disruptive on each tier: the
                                          driver's layer of the classification chain

EnforcementDecl
  enforced      bool
  mechanism     text        "cgroup v2 memory.max", "Windows job object", "none"
  granularity   text        what the mechanism actually bounds, where it differs

SnapshotCaps
  restore_requires   CPU model or template, hypervisor version, host kernel range
                     a snapshot restores only onto a node matching all three
```

> Second external review, finding S51. Recorded in §17 of `23-walkthroughs.md`. `runtime_snapshot`
> was a boolean, and the migration that relies on it (§8 of `05-scheduling.md`) could pick a
> destination where the snapshot cannot be restored: Firecracker restores a snapshot only onto a
> compatible CPU template, the same hypervisor version, and a compatible host kernel. The
> compatibility is now part of the capability, so the scheduler filters destinations by it and a
> live-state migration to an incompatible node falls back to stop-and-start, stated in its plan.

> Added 2026-10-13, from the implementation. §3.3 of `01-model.md` gives the driver a layer in the
> chain that decides whether a step is disruptive (`stop` on a `vm` costs a boot, on a `process`
> it does not), and nothing let a driver say so: every plan used the vocabulary's floor for that
> layer. `disruption` is that declaration, per tier, in the step actions of the plan vocabulary;
> a name outside the vocabulary is refused rather than ignored, because the layer only adds and
> an ignored entry would make a step less disruptive than its driver declared. The control plane
> evaluates it when it computes a plan, from the drivers its nodes last reported, and takes the
> union over every node that serves the workload's organization and offers its tier. A union,
> because the layer may only add, so the strictest declaration is the one a placement cannot
> undercut, and a plan computed before the workload has a node is classified as it would be
> after. What that costs: one driver that declares an action disruptive makes it disruptive for
> every workload of that tier its organization's nodes could run, including those placed on a
> node whose driver does not. The `oci` driver declares nothing, because stopping and starting a
> container costs no more than the process inside it does.

The control plane **asks** and never assumes, following Nomad's task driver interface. A version
number is not a capability statement: two builds of the same driver on different kernels have
different capabilities, and inferring from a version is how a scheduler places a workload on a node
that cannot run it.

### The three fields that stop the interface from lying

> External review, findings R15, R16 and R17. Recorded in §16 of `23-walkthroughs.md`.

`enforcement`, `volume_semantics`, and `devices` are additions, and all three exist because "one
`ResourceSpec`, every driver" is a claim that is false in three specific places. K-3 says a limit
that is not kernel-enforced is not displayed, and K-9 says the interface expresses Windows from the
first version. Those two are only simultaneously true if the interface can say **which limits this
driver can actually hold**.

**`enforcement`, because Windows has no cgroups.** Job objects bound memory and CPU differently.
NTFS quota is per user, not per directory tree, so it does not express "this volume gets 20 GB".
The kernel-level equivalents for IOPS and egress are partial. A Windows driver therefore declares
memory and CPU as enforced by job objects, declares disk quota as `enforced: false`, and the
scheduler refuses a workload that asks for a limit the driver cannot hold, while the interface
**does not display** the ones it cannot enforce. That is K-3 working as designed rather than K-3
quietly suspended for one platform, and it is the honest version of the Windows column §2 admits it
lacks.

Bandwidth is the one limit a driver that joins the node's links (`network_modes: veth`, or `tap`
for a microVM's guest, §4.1 of `07-networking.md`) does not hold itself: the node's network shapes
it, on the workload's link and the node's uplinks (§7 there). So the agent adds `egress` and
`ingress` to such a driver's `enforcement` when its operator has declared an uplink and its helper
can write the qdiscs, and leaves them out otherwise; a driver that builds a network of its own
declares its own. Added 2026-10-21, when shaping was designed.

> Corrected 2026-10-21, when an endpoint's own shaping and accounting were built (P2.8c of the
> build plan): each kind is declared on what it rests on, and only an egress reservation rests
> on an uplink. The agent adds `endpoint_accounting` wherever its helper gives workloads
> networks, since the host's table counts each endpoint; `ingress` and `endpoint_shaping`
> wherever the helper can also write the qdiscs, since both are held on the workload's own link;
> and `egress` only where an uplink is declared and shaped as well, since a reservation is held
> there and a limit is placed against the bandwidth it offers. The paragraph above tied
> `ingress` to an uplink, which nothing held there needs.

**`volume_semantics`, because a VM's volume is a block device.** Under `process` and `container`, a
volume is a directory on a filesystem the host controls, quota is `refquota` or a project quota,
exceeding it returns `EDQUOT` in the write path, and inodes are a thing that can run out. Under
`microvm` and `vm`, the volume is a zvol or a raw image the guest owns. The host cannot apply a
filesystem quota inside it; the bound is the device size, the guest sees `ENOSPC`, and the host has
no concept of the guest's inodes at all. Same `VolumeSpec`, two different contracts, and P4
requires that the difference reach the user rather than being smoothed over. §3 of `06-storage.md`
carries what each one means.

This also sharpens Rule D of `20-roadmap.md`. The second driver is the test of the interface, and
what it is testing is not process supervision, which is easy; it is this, where the storage
abstraction genuinely does not survive contact with a hypervisor.

**`devices`, per tier, because Firecracker has no PCI passthrough.** Bet 2 lists GPU workloads
among the segments microVMs open, and Firecracker's minimal device model does not do passthrough;
that path is `qemu` with VFIO. Without devices in the capability declaration, the scheduler accepts
`microvm + gpu`, places it, and the failure surfaces at apply time. "The control plane asks and
never assumes" has to cover devices for the same reason it covers everything else in this list.

`config_schema` is the honest part of the abstraction. A VM needs a kernel, a bootloader, and
cloud-init; a container needs none of those. Core does not model that difference, the driver
publishes a schema, the recipe and the operator fill it in, and Korpis validates against it without
understanding it. **The lifecycle is common; the configuration is driver-declared.** An abstraction
that claimed to unify the configuration too would be a lie that leaks on the first unusual
workload.

### 4.2 Lifecycle

```
Prepare(Intent)          → PreparedSpec | Rejection    // pure: validates, resolves, no side effects
Create(PreparedSpec)     → RuntimeID
Start(RuntimeID)
Stop(RuntimeID, signal, grace)
Kill(RuntimeID)
Destroy(RuntimeID)                                     // release all resources

Recover()                → []RuntimeID                 // adoption after agent restart
Inspect(RuntimeID)       → RuntimeStatus
Wait(RuntimeID)          → ExitStatus
Stats(RuntimeID)         → ResourceUsage, ObjectCounters

Signal(RuntimeID, SignalName)
Exec(RuntimeID, Command) → Stream                      // capability-gated
AttachConsole(RuntimeID) → Stream                      // capability-gated
Resize(RuntimeID, Resources)                           // capability-gated
```

Three of these carry more weight than the rest.

**`Prepare` is pure.** It validates that this driver on this node can satisfy this intent, resolves
references, and returns either a prepared spec or a rejection with a reason, changing nothing. This
is what lets the Planner produce a plan that is known to be satisfiable before anyone approves it,
and it is what makes dry-run genuinely free (P5). A `Prepare` with side effects would make every
dry-run a partial apply.

**What the `oci` driver's `Prepare` reads** is its arguments and nothing on the node: the intent,
and the recipe document the assignment carries (§4.8 of `02-architecture.md`). From the recipe it
takes the container target for this machine's architecture, pinned by digest, and the entry; from
the intent's resolved configuration, the variables and arguments of §5.2 of `09-recipes.md`; from
the intent, its volumes as mounts whose sources the helper names before `Create`, and its
resources, with the default process limit where none is set (§5.2 of `05-scheduling.md`). It
refuses, naming why, a tier other than `container`, an intent that names another driver or sets
driver configuration (its declared schema admits none), and a recipe with no image for this
architecture. It first also refused a configuration that needed a managed file; since 2026-10-15
the agent renders managed files into the directory it hands the driver as `/run/korpis` (§5.2 of
`09-recipes.md`), and the driver sets nothing for them. It maps no ports: the container's network
held only loopback until the networking of Phase 1, the driver declared `network_modes: none`, and
an endpoint stayed without an address rather than claiming one. It declares `veth` and `none` since
that networking was built: a container joins the network namespace the helper made for its
workload, and the node, not the driver, forwards its endpoints' ports (§4.1 of `07-networking.md`).
Corrected 2026-10-19, as Phase 1 closed; both passages still described the Phase 0 driver.

**The `oci` driver declares the console modes `none`, `logs`, and `interactive`** (Phase 1). For
any console mode with a stream, `Prepare` returns the stream's limits and the secret values it
redacts as the spec's stream capture, and the agent fills in its directory, a volume the helper
creates before `Create` (§4 of `14-streams.md`). The driver writes the task's stdout and stderr
through containerd's logging binary, `korpis-stream-writer`, which the shim starts beside the task
and which outlives the driver as the task does. It refuses a stream directory that is not the
workload's own stream volume, and refuses to create a container with a stream when it was started
without a writer, since starting it would lose its console silently. An `interactive` console runs
the task under a terminal, whose output reaches the writer through containerd's console copy, and
gives the task a stdin FIFO the writer holds open for the task's whole life. `ConsoleInput` sends
input on a second FIFO to the writer, which records it with who sent it before feeding it to the
workload, and `ResizeConsole` records the new size the same way and resizes the task's terminal
(§6 of `14-streams.md`). Neither waits on the writer: input that finds it gone, or the workload not
reading, is refused or recorded as dropped, never left to stall the call.

**The `firecracker` driver declares the same three console modes** (Phase 3), through the one
console a guest has, its serial port, which is its Firecracker's standard input and output. The
driver has no shim to start the writer, so it starts `korpis-stream-writer` itself for each run,
before the guest, handing it the guest's output on the descriptors containerd's shim would; once
the jailer has made the guest's cgroup the writer is moved into it, so it is counted with its
guest and outlives a restart of the driver, and it seals and exits when the guest's Firecracker
does. It refuses the same stream directories, and the same secrets, as the `oci` driver; what both
share of a console is one module of `korpis-driver-kit`. Inside the guest, `korpis-guest` gives a
console of logs the port's output as the entry writes it, with the terminal's output processing
off, so a line holds no carriage return a container's would not; an interactive entry runs in a
session of its own whose controlling terminal is the port, as a container's runs under its
terminal. A guest's kernel clears the port when it opens it at boot, so what is typed before then
is lost: `korpis-guest` says on the config drive that the run has started once the port is open,
and the driver holds an input until it has, within ten seconds (Finding 146 of
`23-walkthroughs.md`). What it does not do, stated: `ResizeConsole` is refused, since nothing
carries a terminal's size into a guest until its agent does (§4.4), so a full-screen program in an
interactive guest sees the port's default size; and the console carries what the guest kernel
prints at its quiet level (§6).

**`Wait` is a stream of events.** The protocol carries it as `Events`: for every task of the
driver's objects that starts, exits, pauses, resumes, is deleted, or runs out of memory, the status
it leaves the object in, read back with `Inspect` rather than built from the event. An event can
therefore only make the supervisor look sooner, never tell it something `Inspect` would not, and
one lost in a reconnection costs that latency and nothing else, because the reconciler reads every
object on its period anyway (§4.1 of `02-architecture.md`).

**`Stats` answers what metering sums, as the kernel counts it.** Beside the usage now, a rate
over the time since it was last asked and the memory in use, it answers the object's counters
since the object was made: CPU time and time throttled, bytes read and written, memory in use and
its high-water mark (§2 of `15-observability.md`). Each is absent where the driver cannot read
it, and the `oci` driver reads them from the container's cgroup v2 files, so a controller not
enabled is a quantity not measured rather than a zero. The counters begin again with each object;
keeping a workload's continuous across its objects is the agent's meter, not the driver's. Added
2026-10-21, when metering was built: `Stats` answered only the usage now, from which no interval
can be summed.

**`Recover` is not optional.** It is the adoption primitive of §4.4 of `02-architecture.md`. A
driver that cannot enumerate and re-adopt the objects it created is a driver that turns every agent
restart into a customer outage. It belongs in the interface, not in each implementation's good
intentions.

> Added 2026-10-23 (Finding 144 of `23-walkthroughs.md`). `Stop` and `Signal` name a signal, and
> nothing said how one reaches a workload that shares no kernel with its driver. A driver
> delivers what it can and declares exactly that in `signals`: one whose workloads are a guest's
> processes may declare no names at all, and then its graceful default and its kill are the only
> two things it tells a workload, in that order. §6 says how the `microvm` tier delivers both.

**`Create` and `Start` are separate.** Resources are allocated, volumes mounted, and the network
attached before anything executes. This makes failures happen at a point where nothing is running
yet, and it is a prerequisite for Windows, where creating a job object and starting a process in it
are genuinely distinct operations.

### 4.3 What the interface must never assume

This list exists because these assumptions are invisible until they are load-bearing, and each one
would silently close a tier. Rule K-9 is satisfied by this list, not by intent.

| Never assume | Instead | Closes if violated |
|---|---|---|
| POSIX signal numbers | driver-declared signal **names** | Windows |
| uid/gid identity | an opaque `SecurityContext` the driver interprets | Windows |
| cgroups | resources declared semantically (bytes, millicores, IOPS); the driver maps them | Windows, VMs |
| a PID identifies the workload | `RuntimeID` is an opaque driver-issued string | VMs, Windows |
| one filesystem tree the agent can assemble | mounts declared as specs the driver realizes | VMs, microVMs |
| the workload is a process the agent's kernel can see | liveness comes from `Inspect`, never `kill(pid, 0)` | VMs, microVMs |
| a command line starts the workload | the rootfs source declares its own entry semantics | VMs, ISOs |
| the agent shares a filesystem with the workload | file access goes through the workload's own worker, on the host or in a guest (§4.4, §6 of `02-architecture.md`) | microVMs, VMs, Windows |
| a uid on disk means the same thing on every node | workload-relative ids, mapped at mount (§4.5) | migration, every tier with user namespaces |
| stdout/stderr are the output | console mode is declared; serial and VNC are console modes | VMs |

The file-access and output rows are the ones that quietly fail. An agent that reads a workload's
files by opening a host path works perfectly for containers and is impossible for a microVM, and by
the time anyone notices, that assumption has spread through the file manager, the backup system, and
the recipe installer.

### 4.4 File access across tiers

> Second external review, finding S40. Recorded in §17 of `23-walkthroughs.md`.

The row above said file access "goes through the tenant's own worker", and §6 of
`02-architecture.md` put that worker on the host, in a mount namespace, using `openat2`. For a
`block_device` volume (§3.1 of `06-storage.md`) there is nothing on the host to open: the guest
formatted the device and has it mounted, Firecracker has no shared filesystem, and mounting the
image on the host while the guest runs corrupts it. Mounting it on the host at all hands the host
kernel's filesystem parser to whoever formatted the image, which for `microvm` is precisely the
tenant the tier exists to keep away from the host kernel. So the file manager, SFTP, in-place
recipe edits, and single-file restore had no mechanism for half the tiers.

**The worker for a block device always runs on a guest kernel, never on the host's:**

| Workload state | Where the filesystem worker runs | Reached by |
|---|---|---|
| running, `microvm` | inside the workload's own guest, started by `korpis-guest` | vsock |
| running, `vm`, with `korpis-guest` installed | inside the guest | virtio-serial or vsock |
| running, `vm`, without it | nowhere: no file access while running, and the interface says so | none |
| stopped, any block-device tier | inside a **service microVM**: a disposable Korpis-built guest that attaches the volume read-write, runs the worker, and is destroyed when the session ends | vsock |

- **`korpis-guest` is the in-guest component**, a small static binary that Korpis places in every
  `microvm` it builds, because Korpis builds the microVM's root filesystem and init from the
  recipe's image (§6). It runs the filesystem worker, answers `guest_agent` health probes (§5), and
  performs clean shutdown. It runs as an unprivileged user inside the guest and enforces the same
  `openat2`/Landlock confinement there, so the tenant's own guest kernel is the one at risk if it
  is attacked, which is the tenant's own business.
- **For `vm`, installing it is the customer's choice**, because a `vm` runs an operating system the
  customer installed. Open question 3 records what that costs.
- **The service microVM is the only thing that ever parses a tenant-formatted filesystem**, and it
  does so in a guest kernel behind the hypervisor boundary. The same mechanism performs single-file
  restore from a block-device backup (§5.4 of `06-storage.md`) and recipe installs into a stopped
  `microvm`'s volume. A node that cannot run microVMs cannot host `block_device` workloads in the
  first place, so the requirement adds nothing a node did not already need.
- **A filesystem the service microVM cannot read** (an encrypted volume, a filesystem type its
  kernel lacks) offers whole-image operations only, which the driver declares at creation rather
  than the user discovering at restore time.

### 4.5 A uid on disk must mean the same thing everywhere

> Second external review, finding S50.

`container` always uses user-namespace remapping (§2) and the install DSL spoke of "the tenant's
uid range" (§4 of `09-recipes.md`, since corrected to the workload-relative ids below), but nothing
said where ranges come from. If each node allocated its own, a workload migrated by `zfs send` or a
file-level sync would arrive with its files owned by host uids that mean another workload, or
nobody, on the destination.

- **Ranges are allocated by the control plane, per workload, unique on the node it is placed on**,
  recorded in the assignment (`ids`, §4.8 of `02-architecture.md`), and given back when the
  workload is retired.

  > Corrected 2026-10-13, from the implementation. This said ranges were unique cluster-wide and
  > never reused while a volume carried them, and the assignment had no field to record one. Both
  > halves of the cluster-wide rule are answered by the next item instead: files carry
  > workload-relative ids, so ownership on disk does not depend on the host range, and the
  > fallback shifts ownership at copy time, from whatever range the source used to whatever range
  > the destination gives. What a range has to be is distinct from every other range **on the
  > same kernel**, which is isolation; distinct across machines buys nothing. The cluster-wide
  > rule also had a cost nobody stated: ranges of 65,536 ids in a 32-bit id space are at most
  > 49,151 above the host's own ids, so it capped a whole installation at that many workloads,
  > a limit by scale that P10 rules out. Unique per node, the same arithmetic bounds what one
  > machine runs, which no machine approaches. The control plane allocates under the per-node lock
  > it already takes to seal a node's set, the lowest free slot of 65,536 ids from 2^30 upward,
  > above every id a host's own users and the subordinate ranges of its rootless users
  > conventionally take; a range is reused only once its workload is retired, because a runtime
  > object that outlived its assignment, an orphan, still runs under it until then.
- **Files on disk carry workload-relative ids** (0 to 65535 as the workload sees them), and the
  range is applied at mount time with idmapped mounts, so on-disk ownership does not depend on which
  host range the workload holds on which node. Migration copies bytes and ownership survives,
  whatever replication path is used.
- The driver declares the mechanism (`id_mapping`): idmapped mounts where the filesystem supports
  them (ext4, XFS, btrfs, and ZFS from OpenZFS 2.2), and a declared, slower fallback (ownership
  shifted at copy time) where it does not. A storage class that supports neither cannot host
  user-namespaced workloads that migrate, and the scheduler knows it.

---

## 5. Health across tiers

`kill(pid, 0)` is not a health check, and for half the tiers it is not even possible.

| Tier | What "the process is alive" tells you |
|---|---|
| `process` | the process exists, nothing about whether it works |
| `container` | the container's init exists, nothing about the application |
| `microvm` / `vm` | **the hypervisor is running.** The guest OS may have panicked twenty minutes ago |

Rule K-16 requires application-level health, and the tier table is the reason it cannot be
optional. Probes are declared by the recipe and executed by the agent from outside the workload:

| Probe | Applies to | Mechanism |
|---|---|---|
| `tcp` | any | connect to an endpoint |
| `http` | `service` | request a path, check status |
| `query` | `console` | a game or application query protocol, provided by an extension |
| `exec` | tiers with the `exec` capability | run a command, check exit status |
| `guest_agent` | `microvm`, `vm` | an in-guest agent reports over a virtio channel |
| `exit` | `task`, `scheduled` | exit status |
| `heartbeat` | any | the workload calls Korpis; absence is the signal |

TCAdmin distinguishes "service monitoring" from "game monitoring" and charges for the distinction.
It is right that they are different, and there is no reason it should be a premium feature.

> Not built until 2026-10-23, and found that day (Finding 151 of `23-walkthroughs.md`). This
> section, §5 of `07-networking.md`, §4.6 of `02-architecture.md`, and §3 of `22-first-party.md`
> described probes the agent runs, and no phase of §4 of `20-roadmap.md` named running them, so
> none did: through Phase 2 every observation's health was `unknown`, an endpoint was `ready`
> whenever its workload's process ran, and a dependent was created once its database's process
> ran, ready or not. The probes a node runs are built as below, and the kinds it does not run yet
> are homed in Phase 3 there.

**As built.** Decided 2026-10-23. The agent runs a workload's `tcp` and `http` probes while its
runtime object runs. Each attempt is made from a socket the helper opens inside the workload's
network namespace (`OpenProbe`), connected to the workload's address and the port of the endpoint
the probe names. A connection made from inside reaches the endpoint as the workload's own loopback
does, or, for a microVM, as its tap does, so an endpoint whose rules turn away the node's own
address (§6 of `07-networking.md`) is probed all the same, as is one whose exposure is `none`; the
socket reaches nothing the workload itself could not. A `tcp` probe passes when the connection is
made; an `http` probe sends a `GET` of its path over HTTP/1.0 and passes on the status it expects,
or any 2xx where it expects none; each attempt, connecting included, is held to the probe's
timeout. A probe that does not say is tried every 10 seconds, given 5 seconds and never more than
its period, judged failing after 3 attempts in a row fail, and first tried as its object starts; a
period under a second is taken as one second, so that no recipe makes the prober a load on what it
judges. A probe passes at its first success and fails only after its `failures`; before either it
is unjudged. Each start of the object, and an object built from another intent, is judged afresh.

The verdicts decide three things. Readiness decides whether the workload's endpoints are `ready`,
and whether a dependent that requires it is created (§5 of `07-networking.md`). The health the node
reports is the liveness verdict, or the readiness verdict where no liveness probe is declared, and
`unknown` while unjudged or while the object does not run; a change of it is the `health.changed`
event (§5 of `15-observability.md`). A liveness probe that fails restarts the workload as a crash
does: the same backoff, a second doubling to five minutes and forgiven after a minute up, counted
with its crashes, and recorded as `node.restart` with the cause `unhealthy` and the probe's kind;
under a lease that has expired, only where the workload keeps running (§4.6 of
`02-architecture.md`).

What it does not do yet. A `query` probe needs its protocol's provider extension, a `heartbeat`
probe the workload calling Korpis, and a `guest_agent` probe the guest's channel to its agent; each
is built later in Phase 3 (§4 of `20-roadmap.md`), and until then such a probe is unjudged: the
workload's health is `unknown` and its readiness is its object running, as before. An `exec` probe
needs a driver that runs a command inside its workload, which neither driver does yet, so every
node whose driver does not declare `exec` refuses such a workload at placement (the `probe` check
of §2.1 of `05-scheduling.md`), which until one does is every node; that is said rather than a
probe nobody runs. Where a node gives its workloads no network, it runs no probe. What it costs:
one connection a period for each probe of each running workload, and a call to the helper for each
socket; and a workload whose liveness probe fails a minute after each start is restarted every
minute and never backs off further, as one that crashes a minute after each start is.

---

## 6. Content and rootfs

The **agent** owns the node-local content store, not the drivers. Recipes, images, and disk images
are fetched once per node, verified against their digest, and shared by every workload that
references them (Rule K-15).

This is TCAdmin's patch system (download from Steam once, distribute across the network)
generalized and made automatic. Pterodactyl downloads the same bytes again for every server,
forever.

Drivers **reference** content by digest and never fetch it themselves. Consequences:

- One fetch, one verification, one signature check per digest per node, regardless of driver.
- **Fetch sharing across tiers**: a `container` and a `microvm` built from the same base share the
  downloaded layer blobs, so the bytes come over the network once. They do not share realised
  storage: an unpacked overlay tree and a guest block image have no blocks in common, and
  Firecracker has no shared filesystem to make them. The first version said they "share storage",
  which overstated it (finding S53).
- A compromised driver cannot pull arbitrary content; it has no registry credentials and no network
  path to a registry (§7 of `02-architecture.md`).
- Prefetch is a scheduling concern: the control plane can hint a digest to a node before placing a
  workload there, so start latency is not a download.

**How the store is kept** (2026-10-13, from the implementation). Content is fetched by digest
only: a reference without one is refused, because a tag is a question the registry may answer
differently tomorrow. An index is resolved on the node to the manifest for the node's own
platform. Every blob is written to an ingest file that refuses bytes beyond the size its
descriptor declares and is hashed as it is written, then synced, then renamed to a file named by
its digest, so a file in the store is always whole and always what its name says; a name is
checked to be a well-formed digest before it becomes a path. A driver reads the store and never
writes it, and verifies again what it imports, since containerd checks every blob it commits
against the digest it is given.

**How the `oci` driver hands content to containerd.** It writes each blob from the store into
containerd's content store, commits the manifest with the garbage-collection labels that tie its
configuration and layers to it, and names the image, so what was imported stays until the image is
removed. It unpacks each layer with containerd's diff service into a snapshot named by its chain
identifier, so images that share a base share its snapshots, and refuses a layer that does not
unpack to the uncompressed digest the image configuration names. All of it happens under a
containerd lease taken for the create, so nothing half-assembled is collected and nothing is left
behind if the create fails. The container's own snapshot is prepared remapped onto the workload's
id range (containerd's `uidmapping` and `gidmapping` labels), which is what lets a read-only image
owned by host root be the root filesystem of a process whose root is an unprivileged host id.
Every object lives in containerd's `korpis` namespace and nothing outside it is read or written.

**A `microvm` boots the node's kernel, and its first process is Korpis's** (decided 2026-10-23,
when the guest was built for the `firecracker` driver). The kernel is built by
`scripts/guest-kernel.sh` from a kernel.org longterm release pinned by its digest, with
Firecracker's own guest configuration vendored in `guest/kernel/`, and every `microvm` on a node
boots that one. A recipe does not bring a kernel: its image is the root filesystem and nothing
else, and a workload that needs a kernel of its own, a module the configuration leaves out, or
another operating system is a `vm` (§2). Three reasons. The guest's first process relies on the
kernel having what it uses (virtio block devices, ext4, overlayfs, vsock, and the keyboard
controller a stop arrives through), and a kernel a publisher chose could lack any of them and
fail only when the workload is first stopped. A guest kernel with a flaw is then one thing for
the operator to patch on each node, not one per publisher, patched or not. And the same image
serves the `container` and `microvm` tiers unchanged, which is what lets them share its fetched
layers (above). What it costs: every `microvm` on a node runs one kernel version, chosen by the
operator; a newer kernel reaches a guest at its next boot, never while it runs; and a workload
whose software needs a different kernel is outside this tier.

The first process is `korpis-guest`, a static binary on a **boot drive** that holds nothing else
and is shared, read-only, by every guest of the node. The drives are attached in a fixed order,
so the guest can count on their names: the boot drive, which the kernel mounts as its root; the
image, read-only; the **config drive**, on which the driver writes what to run (the entry, its
environment, its user and group as numbers, its working directory, and its addresses and
gateways); then each volume. `korpis-guest` assembles the workload's root as the
container tier's is (§2): the image beneath a small tmpfs, in an overlay, so the mount points the
image lacks can be made, and then the overlay read-only, with `/tmp`, `/run`, and `/dev/shm` as
small tmpfs mounts. It mounts each volume's filesystem, sets `eth0`, and runs the entry as its
user in a process group of its own, reaping whatever the entry's children leave. When the entry
ends, it ends every other process and writes how the run ended in the config drive's last block,
then restarts the guest, which ends the microVM: on x86 Firecracker exits when its guest resets
the CPU, and has no power-off to offer (§5 of `research/evidence.md`). A **stop** is
Ctrl+Alt+Del, which Firecracker injects through the same keyboard controller; the guest's first
process receives it as `SIGINT` and sends the entry's group `SIGTERM`, and a second Ctrl+Alt+Del
ends every process at once. The guest keeps no grace of its own: the grace is the supervisor's
(§6 of `02-architecture.md`), whose stop signals the object with the driver's graceful default and
kills it once the grace is over, so the driver's default is the first Ctrl+Alt+Del and its kill
the second, after which the guest syncs its volumes and restarts. Since a guest counts what it is
sent, the driver keeps the count honest at both ends (Finding 145 of `23-walkthroughs.md`). A stop
is sent once in a run: an agent restarted in the middle of a stop asks for it again (§6 of
`02-architecture.md`), and a second Ctrl+Alt+Del would be read as the kill, so the driver marks
beside the object that its guest has been told, and clears the mark when it starts the guest
again. A kill sends Ctrl+Alt+Del every quarter second until the guest has ended, because a kill may
come with no stop before it, as a destroy's does, and because one that arrives before the guest
has read the last is absorbed by it (the kernel keeps one pending signal of a kind); a guest that
has not exited within a bound of five seconds is ended with its Firecracker process. The host
reads back sixteen bytes of fixed layout, the exit status or the signal, and never a filesystem
the guest has written (§4.4). The driver clears them before every run, and the guest first writes
there that the run has started, once its serial port is open (§3; Finding 146 of
`23-walkthroughs.md`).

What the guest does not give, stated: inside it the entry runs as its user with no user namespace,
so the image's uid 0 is root of a kernel that runs nothing but this workload, which is the
boundary the tier draws; the console carries what the guest kernel prints at its quiet level beside
what the workload prints; and the image's user must be a number, as for the container tier (§2).

**How a `microvm`'s image drive is made** (2026-10-23). With no containerd to unpack for it, the
driver has the image's layers applied by `korpis-image-build`, and the layers are a publisher's
bytes, so the process that reads them holds no privilege. The build runs as two processes. The
first, root, reads the image from the node's content store, its manifest and configuration checked
against their digests, and makes a work directory owned by the workload's range of ids. The worker
drops its groups and makes its own user, network, and IPC namespaces in a pid namespace of its own;
the first maps its ids from 0 to the workload's 65,536 and streams it each layer, checked against
its digest as it goes. The worker becomes its namespace's root and applies the layers as the image
layer specification says (§5 of `research/evidence.md`): an entry replaces what was at its path,
`.wh.NAME` removes the `NAME` a lower layer made, `.wh..wh..opq` hides what lower layers put in its
directory, and every path is resolved by the kernel as if the tree were the root
(`RESOLVE_IN_ROOT`), so an earlier layer's link, absolute or climbing, is followed within the tree
and never out of it. Then it runs `mke2fs -d` on the tree in the same namespace, so the drive's
files are owned by the workload's ids from 0, as they are for the container tier on disk (§4.5),
and the first makes the drive root's, read-only, and named by the manifest's digest, which a later
workload of the same image reuses. The boot drive is made the same way minus the worker, since
nothing in it comes from a tenant, and is named by the digest of the `korpis-guest` it holds. What
it costs: a node holds each image twice, its layers in the content store and its drive beside them,
and builds the drive once before the image's first guest can start; an entry owned by an id beyond
the workload's 65,536 refuses the image, as the container tier's id mapping would; device nodes in
a layer are skipped and counted, since the guest's `/dev` is its kernel's; and extended attributes
are not kept, so a binary that relies on a file capability (`ping`, in some images) runs without it
in this tier.

Built so far (Phase 3): the kernel, `korpis-guest`, the config drive's format, the image and boot
drives, and the `firecracker` driver as a process of its own. Each guest's Firecracker is started
by Firecracker's jailer in a chroot holding hard links to its drives, as its workload's first id,
in a pid namespace and a cgroup of its own whose `memory.max`, `pids.max`, and `cpu.max` hold the
guest's shape, and it outlives the driver: a restarted driver finds every guest from the record it
keeps per object, and watches each again by a descriptor of its process, so a process id reused is
never mistaken for it. The jailer forks to do this, so the process the driver watches is the one
the jailer names in its chroot, and never the one the driver started. A guest's Firecracker can
write its chroot, and a guest that escaped into it would write there whatever would mislead the
driver, which is root; so nothing in a chroot is trusted (2026-10-23, from a review of the first
build). The chroot is built afresh for every run, so neither the jailer nor the driver reads what
an earlier run left; the process the jailer names is believed only if it runs as the workload's id
in the guest's cgroup, which only root can move a process into, since the driver kills what it
records (the jailer names its child as soon as it has cloned it, before the child drops to that id
to run Firecracker, so a process named but still root is waited for within the jailer's bound, not
refused: found 2026-10-23, when a start lost that race); and the API socket is opened as itself,
never through a link, so it cannot be made to name another socket on the host. It is proven by
guests run, stopped, killed, recovered, and given a console (`crates/korpis-guest/tests/boot.rs`,
`crates/korpis-image/tests/build.rs`, `crates/korpis-driver-firecracker/tests/`), the last through
the agent with the `oci` driver beside it, and end to end on a node joined to a control plane that
runs both drivers, where a container and a guest are placed side by side, the guest's console is
read with `korpis logs`, its endpoint answers through the node's port, its volume keeps what it
wrote across a restart, and its kernel sees no more memory than its limit
(`crates/korpis-e2e/tests/e2e/microvm.rs`); the console is described in §3. A volume's drive is the
image the helper makes in it (§3.1 of `06-storage.md`), which is on the node's storage, where a
hard link from the driver's state cannot reach: it is bound into the chroot instead, and unbound,
without following a link, when the jail is removed. The driver takes nothing else under the volume
root as a drive: only `<volume>/block.ext4` directly beneath it, with no link on the way, in a
directory the helper marked as a block device's, given to the workload's first id and bound in
through a descriptor of the file it checked, so the file given and bound is that one. A guest's
network is its workload's (§4.1 of `07-networking.md`): the helper makes a tap beside the
namespace's link, the jailer joins that namespace (`--netns`) before it starts Firecracker, which
opens the tap as the guest's one interface, and the agent gives the driver the workload's addresses
with the namespaces, which the config drive carries to `korpis-guest`; the driver joins no
namespace but its workload's own under the helper's root, and takes addresses only with one. So the
driver declares `network_modes: tap`, and a guest is reached at its endpoints as a container is
(`crates/korpis-driver-firecracker/tests/network.rs`). Corrected 2026-10-23: this said a guest had
no network, the driver refused a spec that named one, and no node could place a workload on
`microvm` until the helper gave a guest its tap; that was so until then.

---

## 7. What is deliberately not in the interface

**Live migration is not a driver method.** It is an orchestration built from primitives
(`runtime_snapshot`, volume snapshot, content pre-staging, lease transfer) sequenced by the agent
and control plane. Making it one call would mean each driver reimplements the resumable,
verifiable, reversible state machine Rule K-4 requires, and would then implement it differently and
wrongly. The driver contributes capabilities; `05-scheduling.md` owns the sequence.

**Networking is not a driver method.** Drivers declare which network modes they support and attach
to an interface the agent has already created. A driver that built its own networking would mean
six implementations of endpoint semantics, TLS, egress policy, and traffic accounting.
`07-networking.md` owns that.

**Backup is not a driver method.** It follows from the storage class, not the runtime (Rule K-5).
`06-storage.md` owns it.

The pattern: a driver owns the **execution boundary** and nothing else. Every time something else
is pushed into the driver interface, it becomes something that must be implemented N times and will
behave N different ways.

---

## 8. Conformance

Rule K-7 requires a third party to be able to write a conforming agent. The same must hold for
drivers, or the interface is documentation rather than a contract.

A published **conformance suite** defines what a driver must do:

- every lifecycle method, including failure paths and double-invocation
- `Recover` after a supervisor kill, an OOM kill, and a host reboot, a driver that adopts correctly
  after a clean restart and not after a crash is worse than one that never adopts, because it fails
  only in the situation that matters
- capability honesty: a driver claiming `hot_resize` is tested resizing under load
- resource enforcement: declared limits are verified against actual kernel or hypervisor
  enforcement (Rule K-3, a limit that is not enforced is a lie in a text field, and this is where
  that is caught)
- isolation: tier-appropriate escape attempts must fail

A driver is conforming if it passes. That is the whole gate. This is how an ecosystem of drivers
exists instead of a series of forks.

---

## 9. Open questions

1. **GPU allocation model. Resolved in §6 of `05-scheduling.md`**: devices are discrete named
   objects with declared modes. Passthrough is exclusive, mediated devices (vGPU) partition, and
   timeslicing oversubscribes. These have different scheduling semantics, a GPU is a resource
   dimension in `Quota`, a placement constraint, and a driver capability at once, and it was not
   clear it could be one field.
2. **Nested virtualization.** If a node is itself a VM (which is common), `microvm` and `vm` tiers
   require nested virtualization that many providers disable. The scheduler must know this, so it
   is a driver capability determined at runtime rather than a static declaration. → here
3. **Who owns the guest agent for `vm`?** In-guest health, hot resize, and clean shutdown all need
   one. Building it means shipping software into customer VMs and maintaining it across guest
   operating systems. Not building it means VMs are second-class. Neither is comfortable. For
   `microvm` it is settled: Korpis builds the guest and ships `korpis-guest` in it (§4.4). For
   `vm` the question now has a sharper cost attached, because without it a running `vm` has no file
   access at all. → here
4. **`process` tier and recipes. Resolved in §3 of `09-recipes.md`**: the OCI format, unpacked
   rather than run, so one content store and one signing path. The `native` driver has no image; a
   recipe for it must deliver a directory tree and an entry point.
5. **Windows content store.** Windows container images and Linux OCI images share a format but not
   layers or storage drivers. Whether one content store abstraction covers both, or a Windows node
   runs a parallel one, affects Rule K-15's "fetched once per node". → here, when `winsilo` is
   designed
6. **Windows, as a whole.** Four documents routed their Windows questions here and this list did not
   carry them (finding S18): isolation parity (§13 of `17-security.md`, question 5), node operations
   (§10 of `18-operations.md`, question 4), and the driver language (§10 of `21-stack.md`, question
   3, now Rust by default), alongside question 5 above. They are one question, which is what the
   Windows column of every table in this document says once a Windows machine exists to write it on
   (Phase 5, §4 of `20-roadmap.md`). Until then the interface keeps them possible (§4.3) and nothing
   claims them. → here, in Phase 5
7. **Confidential-computing tiers.** AMD SEV-SNP and Intel TDX would let a tenant's memory stay
   opaque to the host, which no other tier offers. Routed here from §13 of `17-security.md`
   (question 3), and not carried until now (finding S18). It would be a fifth tier with its own
   honest table row, and whether it is worth it depends on attestation (§13 of `17-security.md`,
   question 2), which is the harder half. → here, after attestation is settled
