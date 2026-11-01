# Driver conformance

What a [runtime driver](runtimes.md) must do to conform, and how a third party proves it. A
published suite is what makes the driver interface a contract rather than documentation: a driver
that passes conforms, and that is the whole gate, which is how an ecosystem of drivers exists
instead of a series of forks.

## Contents

- [The suite](#the-suite)
- [Lifecycle](#lifecycle)
- [Recovery](#recovery)
- [Enforcement](#enforcement)
- [Exec](#exec)
- [Dedicated cores](#dedicated-cores)
- [Isolation](#isolation)
- [The runner](#the-runner)
- [What the second driver changed](#what-the-second-driver-changed)

## The suite

`korpis-driver-conformance` is Apache-2.0, as the protocol it tests is. It speaks `korpis.driver.v1`
over the driver's socket and links nothing else of Korpis's, so a driver in any language is tested
as a node uses it. It stands in for the node, given what a node gives: how the driver is started,
its socket, and its paths. It starts the driver itself, so it can kill it as a crash does.

One path is the content store, blobs named by digest under `blobs/sha256/`, written whole and
renamed into place; the suite writes it and the driver only reads. The other is the volume root,
each volume a directory given to its workload's first id, or for a driver declaring block devices, a
directory holding `block.ext4`, a sparse image formatted empty as ext4, marked by the extended
attribute `trusted.korpis.volume` valued `block_device`. Into each object it fills what the agent
would: the labels adoption matches, an id range of its own, each volume's source. Its objects run a
static program of its own, `korpis-conformance-probe`, packed into a one-layer image in a recipe at
the driver's first declared tier.

Each case ends in a pass, a failure saying what was seen, or not applicable, naming the capability
the driver does not declare, which never counts as a pass. A driver conforms when no case fails. The
suite runs as root (it sets a `trusted` attribute and gives volumes to ids), needs `mke2fs` for a
driver declaring block devices, and makes and destroys objects through the driver, so it runs
against a node holding nothing else.

## Lifecycle

`Describe` says a name, a protocol, a tier and its health. `Prepare` changes nothing, answers the
same twice, and rejects rather than fails a tier not declared and an intent naming another driver.
`Create` runs nothing, makes one object however often asked, and refuses a volume outside the volume
root, leaving nothing behind. `Start` runs it, says when, and changes nothing asked again. A `Stop`
reaches the entry well within its grace and kills one ignoring it only once the grace is over.
`Kill` ends it. An exit is inspected with its status and time, and sent as an event. `Destroy`
releases it. Each verb naming an object the driver does not hold is `NotFound`.

## Recovery

A driver that adopts correctly after a clean restart and not after a crash is worse than one that
never adopts, since it fails only when it matters. So the driver is killed with `SIGKILL` while it
holds an object running, one exited and one created, and once started again lists each with its
labels and as it was, the running one with its own start and process, and drives it: a stop reaches
the entry and is sent as an event. An object taking memory past its limit is ended by its kernel,
says `oom_killed`, and is listed so again after a kill.

A host restart is what the fixture says it is: a program run between killing the driver and starting
it, given each object's process, which ends what a restart ends; after it every object is still
held, none runs, and one starts and stops as before. Without the program the case is not applicable.
For Korpis's drivers on the build machine it kills a container's process and shim and a guest's
Firecracker and leaves containerd running, since other services share it, so containerd's own
restart and a `/run` emptied by a reboot are not exercised.

## Enforcement

Each limit a driver declares enforced is met inside one of its objects and found held where the
kernel or hypervisor holds it, tested rather than trusted; a limit declared not enforced is not
applicable, never a pass. Memory, processes and disk are met from inside through `Exec`: memory
taken a mebibyte at a time is ended by the kernel at no more than the limit and at least a quarter
of it; threads started past the process limit are refused below it; a volume fills to no more than
its size before a write is refused. Processor time is read from the driver's own counters, since a
guest's clock does not count time it is not given: four busy threads get no more than the limit,
with a tenth's room for a quota kept per period, and at least half. A driver declaring a resource
resized while it runs fails, since the protocol has no verb to ask it.

The bounds are loose enough to pass a limit enforced a little high, which is what a per-period quota
and a guest's own kernel make of every limit. A driver not declaring `exec` is not applicable for
memory, processes and disk.

## Exec

The probe running as the entry writes who it is once it has taken the terminating signal: its user
and groups, capabilities, whether it may gain privileges, seccomp mode, namespaces, security label,
cgroup, resource limits, working directory and environment. A command run through `Exec` writes the
same of itself, and the two must match line for line. A command ignoring the terminating signal ends
within ten seconds of a two-second timeout, says it timed out, and no longer runs. Output within the
limit arrives whole; past it exactly the limit arrives, said to be truncated, and the command still
ends of itself; a limit past 64 KiB is refused or held to it. A driver not declaring `exec` must
refuse it.

These cases compare what a process reads of itself in `/proc`, so a difference the kernel does not
show there is not seen, and a driver whose objects have no `/proc` fails rather than being compared.
A guest keeps no cgroup of its own, its limits being the host's, so there both say none.

## Dedicated cores

Where a driver declares dedicated cores held, the suite reads the machine's last core from the
topology, never the core of CPU 0, and keeps the cpusets the helper would under the driver's
declared cgroup parent while an object holds it, putting each file back afterwards. An object given
the core must be seen from outside in a cgroup under `dedicated` on exactly those CPUs, with
`shared` on none of them, and from inside able to run on exactly those CPUs (or as many, in its own
kernel); an object given the core of CPU 0 must be refused at `Create`. A machine of one core is not
applicable. On the build machine both drivers run with `--dedicated-cores-host shared`, since a
partition would take CPUs from its other services, so a driver's partition is tested only on a
machine whose host may give one up.

## Isolation

The isolation cases, escape attempts fitting each tier, are not built: their design is held for the
maintainer. Until they are, a driver's conformance says nothing of its isolation, and every report
says so in a line of its own naming the requirement not tested.

## The runner

A third party runs `korpis-driver-conformance` as root with a JSON fixture: `driver` (the program
and its arguments), `socket`, `host_restart` (optional, same shape, given each object's process id),
`content`, `volume_root`, `probe` (the static `korpis-conformance-probe` for the machine),
`uid_base`, and `uid_count` and `objects`, defaulting to 65,536 and 64. An unknown field is refused,
so a misspelt one is never silently a default. It prints each verdict, writes a JSON report where
`--json` says (the suite's version, protocol, driver, whether it conforms, each case with its
verdict and what was seen, and each requirement not tested), and exits 0 when the driver conforms, 1
when a case failed, and 2 when it could not run. Korpis's own drivers are run by it from fixtures
the end-to-end test `crates/korpis-e2e/tests/e2e/conformance.rs` writes.

| Cases | `oci` | `firecracker` |
|---|---|---|
| lifecycle (14) | pass | pass |
| recovery (3) | pass | pass |
| memory, processor time | pass | pass |
| processes | pass | not applicable: declared not enforced |
| disk | not applicable: the helper holds it per volume | pass |
| resize while running | not applicable | not applicable |
| `Exec` (3) | pass | pass |
| isolation | not tested | not tested |

## What the second driver changed

A second implementation of an interface is its test, and `firecracker` was chosen as the most
different one available: a separate kernel, no shared filesystem, no shared process namespace. What
it changed in `korpis.driver.v1` measures how well the interface was drawn.

It changed nothing of the lifecycle: every verb and state, `Recover` and the labels adoption
matches, `Stats`, signals by name, the content store, and the spec's resources and mounts. A microVM
is prepared, created, started, stopped, killed, inspected, adopted and metered through the calls a
container is. Nor did it change how files are reached: `file_access` named a worker in a guest from
the first version. What it added, each because a guest is a separate kernel:

| Added | Why a guest needed it |
|---|---|
| `addresses` on the spec | a container's are set from outside its namespace; a guest sets its own |
| `Files` | a running guest's files cannot be opened from the host |
| `StartInstall`, `InstallStatus`, `ForgetInstall`, a staged image | the host never parses a filesystem a tenant's guest wrote |
| `guest_filesystems` | a block device's backup is read by path only in a filesystem its guests mount |
| `identity_dir`, `guest_agent`, `PingGuest` | `/run/korpis` reaches a guest over a channel, and whether its agent answers is a probe of its own |

`Exec` and `driver` on the spec were added in the same work for both drivers. Every addition is a
new verb, field or capability, and none changed one the `oci` driver already answered. All but the
first row were predicted by the [assumptions table](runtimes.md#what-the-interface-never-assumes);
the first was not, and is now a row of it. The suite then found the shared contract under-specified
in four places, each now stated with the lifecycle: what a verb on an unknown object answers, when a
run started, why the kernel ended one, and a verb listed that the protocol did not have.

What it does not validate: both drivers are Linux on a Linux host, so the Windows rows stay claims;
and a verb no node calls is tested by the suite alone.
