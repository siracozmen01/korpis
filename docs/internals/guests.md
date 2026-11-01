# Guests

How the `microvm` tier runs a workload in a guest of its own: the kernel and first process, the
drives, how a stop and a signal reach the entry, how the guest's files, identity and console are
reached, and how a block device is read without the host ever parsing it. The tier itself is on
[runtimes](runtimes.md).

## Contents

- [Kernel and first process](#kernel-and-first-process)
- [Drives](#drives)
- [Stopping and signalling](#stopping-and-signalling)
- [The control port](#the-control-port)
- [Identity and managed files](#identity-and-managed-files)
- [Consoles](#consoles)
- [The jail](#the-jail)
- [File access](#file-access)
- [Service guests](#service-guests)

## Kernel and first process

**A `microvm` boots the node's kernel, and its first process is Korpis's.** The kernel is built by
`scripts/guest-kernel.sh` from a kernel.org longterm release pinned by digest, with Firecracker's
guest configuration vendored in `guest/kernel/`; every `microvm` on a node boots that one. A recipe
does not bring a kernel: its image is the root filesystem and nothing else, and a workload needing
its own kernel, a module the configuration lacks, or another operating system is a `vm`. Three
reasons: the first process relies on what the kernel has (virtio block, ext4, overlayfs, vsock, and
the keyboard controller a stop arrives through), which a publisher's kernel could lack and fail only
at the first stop; a flawed guest kernel is one thing for the operator to patch per node, not one
per publisher; and one image serves `container` and `microvm` unchanged, sharing its fetched layers.
The cost: every guest on a node runs one kernel version, a newer kernel reaches a guest at its next
boot, and software needing another kernel is outside this tier.

The first process is `korpis-guest`, a static binary that assembles the workload's root as the
container tier's is: the image beneath a small tmpfs in an overlay, so missing mount points can be
made, then read-only, with `/tmp`, `/run` and `/dev/shm` as small tmpfs mounts. It mounts each
volume, sets `eth0`, enters the root with `pivot_root` rather than `chroot` (the kernel refuses a
user namespace to a process in a chroot, which the file worker needs), and runs the entry as its
user in a process group of its own, reaping what the entry's children leave. When the entry ends it
ends every other process, writes how the run ended, and resets the guest, which ends the microVM: on
x86 Firecracker exits when its guest resets the CPU and has no power-off.

Inside the guest the entry runs as its user with no user namespace, so the image's uid 0 is root of
a kernel running nothing but this workload, which is the boundary the tier draws. The image's user
must be a number, as for containers.

## Drives

The drives are attached in a fixed order, so the guest counts on their names: the **boot drive**,
holding only `korpis-guest` and the programs it runs, shared read-only by every guest of the node
and named by the digest of the `korpis-guest` it holds; the **image drive**, read-only; the **config
drive**, where the driver writes what to run (entry, environment, user and group as numbers, working
directory, addresses and gateways) and the guest writes back how the run ended; then each volume.

**The run's record is sixteen bytes of fixed layout** in the config drive's last block: that the run
has started, once its serial port is open, then the exit status or signal, or that the guest's
kernel killed the entry for want of memory. The driver clears it before each run. The host never
reads a filesystem the guest wrote.

**The image drive is built without privilege over the publisher's bytes.** With no containerd to
unpack for it, `korpis-image-build` runs as two processes. The first, root, reads the image from the
content store, manifest and configuration checked against their digests, and makes a work directory
owned by the workload's ids. The worker drops its groups and makes its own user, network, IPC and
PID namespaces, mapped from 0 onto the workload's 65,536 ids, and is streamed each layer, checked
against its digest as it goes. It applies them as the image layer specification says: an entry
replaces what was at its path, `.wh.NAME` removes a lower layer's `NAME`, `.wh..wh..opq` hides a
directory's lower contents, and every path is resolved with `RESOLVE_IN_ROOT`, so an earlier layer's
link, absolute or climbing, stays within the tree. A header's blank owner, mode or time is zero, as
Go's `archive/tar` (which containerd unpacks with) reads it, so an image the container tier runs
builds a drive too. It then runs `mke2fs -d` in the same namespace, so the drive's files carry the
workload's ids from 0, and the root process makes the drive root's, read-only, named by the manifest
digest for reuse.

The cost: a node holds each image twice, layers and drive, and builds the drive before the first
guest can start; an entry owned beyond the workload's 65,536 ids refuses the image, as the container
tier's mapping would; device nodes in a layer are skipped and counted, the guest's `/dev` being its
kernel's; and extended attributes are not kept, so a binary relying on a file capability (`ping` in
some images) runs without it here.

## Stopping and signalling

**A stop is asked of the guest's agent** on its control port, which sends the entry's group the
signal the stop names, `SIGTERM` by default, as a container runtime would. Where the agent does not
take it, a stop is Ctrl+Alt+Del, injected by Firecracker through the keyboard controller; the first
process receives it as `SIGINT` and sends the entry's group `SIGTERM`, and a second ends every
process at once. The guest keeps no grace of its own: the grace is the supervisor's, whose stop
signals with the driver's graceful default and kills when the grace is over, so the default is the
first Ctrl+Alt+Del and the kill the second, after which the guest syncs its volumes and resets.

Since a guest counts what it is sent, the driver keeps the count honest: a stop is sent once per
run, marked beside the object and cleared at the next start, because an agent restarted mid-stop
asks again and a second Ctrl+Alt+Del would be read as the kill. A kill sends Ctrl+Alt+Del every
quarter second until the guest has ended, because a kill may come with no stop before it and one
arriving before the guest read the last is absorbed (the kernel keeps one pending signal of a kind);
a guest not ended within five seconds is ended with its Firecracker process.

**A named signal** reaches the entry through the agent, which sends only the signals the driver
declares (the `oci` driver's set) to the entry's group, or answers that no entry runs; a signal
other than a stop the agent does not take is an error, never Ctrl+Alt+Del. The driver holds a signal
until the run says it has started, within ten seconds, so one sent at boot is not refused for
arriving early. A stop arriving after the run started and before the entry has, while the guest
makes the root, is kept and passed to the entry's group once it starts, as a container runtime
delivers a stop sent just after a start.

## The control port

A guest has a vsock device whose socket is in its chroot, and `korpis-guest` listens on its control
port, answering at most eight connections at once and closing one silent for half a minute. The
driver connects through the socket as it does to the API socket, never through a link. What the
agent answers is read like the run's record: a frame's kind and length, bounded before anything else
is read, then fields of fixed layout, so the root driver parses nothing the guest could shape. It
answers `PingGuest`, signals and stops, console resizes, install progress, and commands.

**A command** (`Exec`) runs as the entry runs, its user, environment and working directory in the
workload's root, its output passed on in frames the driver forwards unread, then its end in six
bytes of fixed layout. The first process reaps every child, so a command's status is kept for the
thread that ran it, and while a command is being started only the entry is reaped, since the
standard library waits itself for a child whose exec failed. The agent listens before the first
process has entered the workload's root, so a driver told the run has started can reach it at once;
a command or file session asked meanwhile waits until the root is entered and, with an identity
directory, its files have arrived, at most a minute.

A guest whose kernel has no vsock boots and says so on its console, and its `guest_agent` probe
fails. The workload is root of its own guest where its image's user is 0, so it can stop the agent
or answer in its place, which can only misreport its own health.

## Identity and managed files

**A guest's `/run/korpis`** serves `identity.sock` in the workload's root before the entry starts,
and `api.sock` while the node serves the API relay, which the driver looks for every two seconds.
Each connection is relayed byte for byte over vsock to a host port (1025 for identity, 1026 for the
API relay), which Firecracker hands to a socket the driver listens on in the chroot, and the driver
relays it to the workload's own socket on the host. `Create` checks the spec's identity directory is
exactly the workload's own under `--identity-root`, and each start checks again that it resolves
there through no link. Neither end reads what crosses, and a request from inside a guest names the
workload its node made the socket for, nothing the guest chose. Each end relays at most sixteen
connections per guest.

**Managed files and dependency files are sent as copies**, since no host directory can be mounted
into a guest. The driver looks at the workload's `config/` and `deps/` on the host every two seconds
and sends each changed file in parts of at most 60 KiB, then what each directory keeps, the rest
removed; the guest writes a file under a hidden name and renames it into place read-only, so the
workload reads a whole file or the one before. The entry waits for the first sync, at most thirty
seconds. A file is read on the host through no link and only if regular; a directory is sent at most
200 files, a file at most a mebibyte, a guest at most eight mebibytes in all (its `/run` is a
sixteen mebibyte tmpfs), and what is past those is left out and logged. The cost: a change reaches a
guest up to two seconds after it would reach a container; an entry starts a second or two after its
object runs, which a heartbeat probe's initial delay should allow; an entry whose driver restarts as
it boots may start without its files after thirty seconds, and its console says so; and the copies
are read-only by mode, not by mount, so a workload root of its guest can change them, which only it
sees.

## Consoles

A guest has one console, its serial port, which is its Firecracker's standard input and output. With
no shim, the driver starts `korpis-stream-writer` itself before the guest and moves it into the
guest's cgroup once the jailer has made it, so it is counted with its guest and outlives a driver
restart; it seals and exits with the guest's Firecracker. Inside, a console of logs gives the port
the entry's output with the terminal's output processing off, so a line holds no carriage return a
container's would not; an interactive entry runs in a session whose controlling terminal is the
port. The kernel clears the port when it opens it at boot, so the driver holds input until the run
says it has started, within ten seconds. A resize is recorded in the stream with who sent it, then
asked of the guest's agent, which sets the port's size so the kernel sends the entry `SIGWINCH`; a
console of logs has no terminal and its resize is refused. The console also carries what the guest
kernel prints at its quiet level.

## The jail

Each guest's Firecracker is started by Firecracker's jailer in a chroot holding hard links to its
drives, as the workload's first id, in a PID namespace and a cgroup of its own whose `memory.max`,
`pids.max` and `cpu.max` hold the guest's shape. It outlives the driver: a restarted driver finds
every guest from the record it keeps per object and watches each by a process descriptor, so a
reused PID is never mistaken for it.

**Nothing in a chroot is trusted.** A guest's Firecracker can write its chroot, and a guest that
escaped into it would write whatever would mislead the driver, which is root. So the chroot is built
afresh for every run; the process the jailer names is believed only if it runs as the workload's id
in the guest's cgroup, which only root can move a process into (a process named but still root, the
jailer's child before it drops privilege, is waited for within a bound); and the API socket is
opened as itself, never through a link.

A volume's drive is the image the helper makes on the node's storage, where a hard link from the
driver's state cannot reach, so it is bound into the chroot and unbound without following a link
when the jail is removed. Only `<volume>/block.ext4` directly beneath the volume root, with no link
on the way, in a directory the helper marked a block device's and given to the workload's first id,
is taken, and it is bound through a descriptor of the file checked.

**A guest's network is its workload's**: the helper makes a tap beside the namespace's link, the
jailer joins that namespace before starting Firecracker, which opens the tap as the guest's one
interface, and the config drive carries the workload's addresses to `korpis-guest`. The driver joins
no namespace but its workload's own under the helper's root, so it declares `network_modes: tap` and
a guest is reached at its endpoints as a container is. See
[addresses](addresses.md#a-guests-network).

## File access

For a block-device volume there is nothing on the host to open: the guest formatted and mounted it,
Firecracker has no shared filesystem, and mounting the image on the host while the guest runs
corrupts it. Mounting it on the host at all would hand the host kernel's filesystem parser to
whoever formatted the image, which for `microvm` is the tenant the tier exists to keep from the host
kernel. So **the worker for a block device always runs on a guest kernel, never the host's**:

| Workload state | Where the filesystem worker runs | Reached by |
|---|---|---|
| running, `microvm` | inside its own guest | vsock |
| running, `vm`, with `korpis-guest` installed | inside the guest | virtio-serial or vsock |
| running, `vm`, without it | nowhere: no file access while running, and the interface says so | none |
| stopped, any block-device tier | a service guest | vsock |

The worker is `korpis-fsworker`, the one the helper runs beside a container, carried on the boot
drive, so one program parses a session's requests on either tier. The first process holds it open
across entering the workload's root (which does not hold the boot drive) and runs one per connection
on the files port (1027). The driver opens the session through its `Files` stream and sends the
worker's spec first: each volume at its guest path, read-only where the workload mounts it so, and
whether the session writes. The worker does what it does on a host: its own user, mount, network and
IPC namespaces, ids mapped to the guest's 0 to 65,535 (the workload's); a root of a read-only tmpfs
holding each volume, idmapped, and nothing of the image; Landlock; until the session closes. It is
root only of its own user namespace. The `firecracker` driver declares `file_access: guest_worker`,
the node opens a session through the driver, and SFTP becomes a file session at the relay, so it is
served the same way. A session opened between an object's start and the node naming it goes to the
helper, which refuses a block device and is asked again.

For a `vm`, installing `korpis-guest` is the customer's choice, since a `vm` runs an operating
system the customer installed; without it a running `vm` has no file access, in-guest health or
clean shutdown, and building it means maintaining software across guest operating systems. A
filesystem the service guest cannot read (encrypted, or a type its kernel lacks) offers whole-image
operations only, declared at creation rather than discovered at restore.

## Service guests

A service guest is a disposable guest of Korpis's own, booted from the same boot drive with no image
(the boot drive stands in its place, never mounted), its config drive written afresh, no network,
identity directory or terminal. It is the only thing that ever parses a tenant-formatted filesystem,
and it does so behind the hypervisor. A node that cannot run microVMs cannot host block-device
workloads at all, so the requirement adds nothing.

**Files of a stopped guest.** The service guest attaches the object's volumes as its own guest does,
with the object's shape and without its network namespace. Its first process mounts a 1 MiB tmpfs
root holding only mount points, makes it read-only, mounts each volume, enters it with `pivot_root`,
and serves the files port until stopped. There is one service guest per workload at a time, keyed by
the workload rather than its object, since a first install runs before the workload has one. It
serves every session on its workload and is stopped, its volumes synced, with the last: with two
sessions on one object (the panel's file manager and SFTP), one guest per session would attach the
volume to two guests at once and corrupt it. A block device is attached to one guest at a time, so
the object's start ends its service guest and its sessions; the panel reopens a session and the node
opens it on the object's own guest. Refusing the start while a session is open would let a file
manager left open in a browser hold a workload from starting. A restarted driver ends any service
guest the previous one left. The cost: a session waits for a guest to boot; the service guest uses
the object's own memory and CPU, which its reservation already holds; the driver boots or stops one
service guest at a time; and a workload never started has no object to serve, so a session on it
goes to the helper, which refuses.

**Installs.** A workload whose volumes are block devices has its [recipe](recipes.md) installs run
by its driver in a service guest booted for the install. The driver writes the install's spec,
naming the guest's paths, and the content it needs (recipe files and fetched downloads, each by
digest) on a content drive: an ext4 filesystem the host makes from its own files with `mke2fs`,
mounted read-only in the image's place. The guest enters a root of the volumes and runs
`korpis-install-job` from the boot drive, confined as on a host. The driver asks the agent how it
goes, a progress and the job's result, decoded as the helper's is, keeps how it ended, and stops the
guest unless a file session holds it. A file session opened meanwhile is served by the same guest,
one open when an install starts is ended first, and the workload's start is refused while an install
runs, since ending one part way would only make it resume. What the driver keeps of an install is in
memory, so a restarted driver knows none and the node starts it again from the progress it kept.

**Reading a block device's backup.** A backup of a block device is its image (see
[backup](backup.md)), and reading inside it is parsing a filesystem the tenant's guest wrote. The
node, serving `backup.browse` or `backup.read`, has the helper restore the image into a staging
place of its own beside the volume, never exchanged with it, and opens the driver's `Files` stream
on that image. The driver checks it as it checks a volume, and that its filesystem is one its guests
mount (`guest_filesystems`), refusing any other as whole-image only; then it boots a service guest
holding the copy alone, at `/backup`. That guest is keyed by the image, so it runs whether or not
the workload's own guests do, and sessions on the same copy share it. The copy is attached writable
and mounted read-only: an image from a running guest is crash-consistent, its journal may need
replaying, and a kernel refuses to mount such a filesystem from a read-only drive, so the guest's
kernel replays it into the copy, which is therefore never restored from and is discarded. The worker
serves it read-only. The cost: a browse waits for the image to be restored, its whole size less
holes, and for a guest to boot, and the staging place takes that room until the session ends.

**Restoring a path into the volume.** The node stages the image as a browse does while the workload
runs; once it is stopped, the node asks the driver for an install whose work is a restore, and the
driver boots a service guest holding the workload's volumes and the copy, the copy mounted read-only
at `/.korpis-backup`. The job copies the path into a new directory of the volume, reading and
writing each file's data and keeping its holes, since the kernel refuses an offloaded copy between
two filesystems. A file session on that guest reaches the volumes and never the copy. The workload
is down for the copy, which a block device attached to one guest at a time cannot avoid.
