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
Stats(RuntimeID)         → ResourceUsage

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

**`Recover` is not optional.** It is the adoption primitive of §4.4 of `02-architecture.md`. A
driver that cannot enumerate and re-adopt the objects it created is a driver that turns every agent
restart into a customer outage. It belongs in the interface, not in each implementation's good
intentions.

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

- **Ranges are allocated by the control plane, per workload, cluster-wide**, recorded in the
  assignment, and never reused while any volume carrying them exists.
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
