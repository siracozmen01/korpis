# The node agent

How a node is split into processes so that nothing both holds privilege and accepts tenant input,
how it proves who it is, and what it holds. The protocol it speaks is on
[reconciliation](reconciliation.md).

## Contents

- [The processes](#the-processes)
- [A driver guards its own door](#a-driver-guards-its-own-door)
- [Stops never block the reconciler](#stops-never-block-the-reconciler)
- [What a workload is given](#what-a-workload-is-given)
- [Internal protocols](#internal-protocols)
- [Node identity](#node-identity)
- [Node state](#node-state)
- [What a node never holds](#what-a-node-never-holds)
- [Break-glass console](#break-glass-console)

## The processes

The largest class of published Wings advisories is a privileged process performing file operations
on tenant-supplied paths, defending itself by inspecting the strings. The agent is built so that
class of bug has nowhere to live.

```
supervisor       unprivileged, no capabilities: the control stream, the data connections,
                 the reconciliation loop; never touches tenant files
helper           root, tiny, socket-activated, synchronous: a fixed set of verbs (mount,
                 cgroup writes, network namespaces, volumes, snapshots, jobs, workers);
                 no tenant paths, no arbitrary exec
drivers          one process per runtime, each its own user: validate every spec they receive
workers          one per workload with files open: a mount namespace holding that
                 workload's volumes alone; unprivileged; Landlock; openat2(RESOLVE_BENEATH)
ingress proxy    parses requests from the internet: no privilege, no filesystem
```

**No component both holds privilege and accepts tenant input.** The supervisor takes control-plane
input and holds no privilege. The helper holds privilege and takes only structured verbs, naming
volumes by identifier, never a path. Workers take tenant input and hold no privilege, confined by
the kernel to a subtree, so a traversal bug produces `EXDEV`, not a host file.

**File operations happen in a namespace containing only the files they may touch.** Wings works on
every tenant's files from one privileged process, which is how a symlink race in one server's
directory reaches the host. Here the helper starts a worker in a fresh mount namespace holding the
volumes of exactly one workload (`OpenFiles`, `korpis-fsworker`; installs use `StartJob` and
`korpis-install-job`), running as the workload's own ids in a user namespace with the volumes
idmapped into it, under Landlock. One worker per workload, not per tenant: a tenant has no single
mount namespace, a stopped container has none (exactly when people edit configuration), and a worker
spanning a tenant's workloads would have to decide by inspecting paths that a share link for one
server may not reach its siblings. A worker lives for one session, so the cost is a process per
workload whose files are open. The file manager and SFTP are this same worker; SFTP is another
protocol in front of it, served by a relay.

Where a volume is a block device a guest formatted and mounts, the worker runs inside the workload's
own guest while it runs, and in a disposable service microVM when it does not. The host never parses
a filesystem a tenant formatted, which would hand the host kernel's parser to that tenant.

The ingress proxy is started by the helper as the supervisor's user, in a cgroup whose new
connections may leave only through a workload's link or over the mesh to a workload an edge route
names; it receives the listening sockets the helper opened on 80 and 443 (TCP) and 443 (UDP), and
its routes and certificates over a Unix socket, and is restarted if it exits.

## A driver guards its own door

The supervisor holds no capability, but what it can cause is bounded by what drivers will do, and
access to containerd's socket is equivalent to root: containerd creates a privileged container with
the host's root mounted if a client asks. So the boundary is at each driver:

- **Every driver validates every spec against its tier's invariants**, independently of the
  supervisor: for a container, user-namespace remapping, the seccomp profile, `no-new-privileges`, a
  read-only root, the dropped capability set, and no mount but the helper's prepared volume mounts,
  the workload's own identity directory read-only, and what the node's Container Device Interface
  files give a device the spec names, read by the driver itself. A spec an honest assignment could
  not produce is refused and reported as a security event.
- **Each driver runs as its own user** with access only to what its runtime needs, so the union of
  driver privileges never lands in one process.
- **Only the supervisor may ask.** A driver's socket opens to its owner and group, and each
  connection's kernel-recorded peer credentials must name an allowed user before a byte is read.
- **Driver validation code is held to the helper's review standard**, since for the `oci` driver it
  is all that stands between a compromised supervisor and root.

A compromised supervisor can ask for anything and obtain only what an honest assignment could
produce. One part is narrower than it reads: a driver checks a device against the host's files, not
against placements it does not know, so a compromised supervisor can give one workload a device
another holds; that stays within the node's tenants, which a compromised node reaches anyway. It
does not reduce containerd's own attack surface, which [security](security.md) carries.

## Stops never block the reconciler

The reconciler moves through a node's workloads one after another, so a stop it waited in would hold
every other workload up (thirty seconds for an object ignoring its signal, ten minutes for a long
pre-stop hook). A stop is a state kept with the object: each pass moves it on (signalled, then
killed once its thirty-second grace is over) and says whether it stopped, and whatever needed it
stopped (a power stop, a restart, a rebuild, a retirement and its lease release, a run's cancel or
timeout) carries on in the pass that finds it so. An exit reported by the driver wakes the
reconciler at once; the kill comes at most one period (two seconds) late. A signal the driver cannot
deliver is not asked again, and the stop goes on to its kill, so a microVM whose Firecracker stopped
answering is still killed. A state or kill the driver does not answer in time is asked again next
pass, and destroying an object already gone counts as destroyed, so a loaded containerd leaves a
retirement waiting, never half done. The cost: an agent restarting mid-stop forgets and signals
again, restarting the grace.

## What a workload is given

The supervisor makes one directory per workload under its identity root, readable only by its own
user and group, and the driver mounts it read-only at `/run/korpis` in that workload alone. It holds
`identity.sock` (credentials, see [authentication](authentication.md)), `api.sock` for workloads
that reach the API, and `config/`, the managed files the supervisor renders from the recipe. So
`/run/korpis` is everything the node gives a workload that it may read and not write. A request on
the identity socket names the workload the socket was made for; it carries nothing the workload
chose about who it is, and the supervisor keeps no credential afterwards. The directory is mounted
rather than the socket because a restarted supervisor binds a new socket file, which the container
then finds in the same directory. In a microVM, the guest's first process serves the same paths and
relays over vsock, and managed files are sent in as copies when they change.

## Internal protocols

The supervisor, helper, workers, proxy and drivers are separate executables joined by local
sockets, and those sockets carry protobuf schemas in the same published tree as the agent protocol,
under the same compatibility rules, so a third-party driver conforms to the same contract as a
first-party one.

- The executables ship as one signed package and upgrade together, but no order is assumed inside a
  node: supervisor and helper each support the other's current and previous internal version.
- The helper speaks no HTTP: it is synchronous and single-threaded, and an HTTP/2 server is the code
  it should least carry. Each message is a 4-byte big-endian length and one `HelperRequest` or
  `HelperResponse`, at most 1 MiB, one request at a time; an unknown verb is answered unsupported
  rather than misread. systemd passes the connection as standard input, so the one root process runs
  only while asked something. Its peer must be the supervisor's user.
- Workers hold no state, so a running one finishes on the old version. Drivers upgrade one at a time
  and re-adopt their objects through `Recover`, the path an agent restart already takes.

## Node identity

A node enrolls with a single-use, short-lived token that is itself an attenuated grant
(`node.enroll` on one organization, one use), and belongs to that organization from then on, serving
it and every organization below it. It generates its Ed25519 key itself and sends only the public
half, so the private key never leaves the machine. Every session runs over TLS 1.3 to the
control-plane certificate pinned at enrollment, and its first message, `Hello`, signs the node
identifier, a timestamp and a nonce with the node key.

The skew window is five minutes either side of the control plane's clock, and a nonce is remembered
for twice that, so a captured hello cannot open a second session while its timestamp would still be
accepted. Every failure is `UNAUTHENTICATED` and nothing more, except a timestamp outside the
window, which says so: the one refusal an honest agent can act on, by its clock, and `Welcome`
carries the control plane's time so drift is noticed early. A decommissioned node's key opens
nothing and its session ends at once. A node holds one session; a new one replaces the old. A node's
labels are bound at enrollment by the operator, and a hello reporting others changes nothing: a node
able to label itself could attract the workloads of anyone selecting on that label.

The protocol is versioned independently of the control plane and agent. The agent declares its
protocol version and capabilities and the control plane asks, never infers from a version number.
The control plane supports the current and two preceding protocol versions, published. The control
plane upgrades first, then agents one at a time; a mixed fleet is supported, not an emergency.

## Node state

A node has two states that change for different reasons: `admin` (active, cordoned, draining,
decommissioned) when the operator decides, and `connectivity` (registering, connected, unreachable,
unknown) when the network does. Held as one field, a cordoned node that dropped off the network
would come back `ready`, and a network blip would undo the operator's cordon. The `status` a client
shows is derived: `decommissioned` if the node is; otherwise `registering`, `unreachable` or
`unknown` while it is not heard, since that needs attention; otherwise `draining`, `cordoned` or
`ready` from the operator's decision. Only `ready` takes new placements.

```
admin          active <-> cordoned -> draining -> (empty) -> decommissioned
               cordoned and empty, or removed as compromised  -> decommissioned
connectivity   registering -> connected <-> unreachable
               unknown -> connected      (after a control-plane restart, until heard)
```

A node is `connected` from a verified hello and while its heartbeats arrive, and `unreachable` once
three heartbeat intervals pass without one, whether its stream ended or went quiet, since a
half-open stream and a dead machine look the same from the control plane. A control plane that
starts marks every node it recorded as connected `unknown`. Reconnecting returns a node to the
`admin` state it had. `cordoned` stops new placements and leaves running workloads; `draining`
migrates everything off.

A node also reports its capacity and allocatable resources, its drivers with their declared
capabilities (signals, exec, console, resize, snapshot, live migration; the scheduler asks, never
assumes), the devices the Container Device Interface describes, its cores, its storage classes, its
overlay key, port and pools, and whether it holds ports 80 and 443 for ingress. A device or core
carries no record of who holds it; placements do ([resources](resources.md)). There is no location
object: region, rack and provider are labels, and placement selects on them.

## What a node never holds

**No node receives a credential it does not need.** GHSA-pfvc-3p5h-x7h6 is critical because Wings
held registry credentials and a daemon token where tenant templates could reach them. Registry
credentials stay with the control plane's content component, which issues short-lived pull tokens
scoped to one digest per fetch, so a compromised node yields a token pulling one known artifact for
a few minutes. A node never holds a backup repository's credentials or key-encryption key: per
backup job it receives the one workload's data key and a short-lived credential to write, read and
list under that workload's prefix, deleting nothing. A compromised node reaches the backups of the
workloads placed on it, which it could already read live, and no others. The agent holds no grants
and evaluates no policy: it checks token signatures and expiries, since a grant evaluated on a node
would put the authorization engine where a compromised node could reason its way to a conclusion
nobody authorized.

A compromised agent holds a key for its own node, digest-scoped pull tokens, and job credentials for
its own workloads; its supervisor obtains only specs its drivers accept. It cannot read another
node's assignments, mint grants, or write effects attributed to another actor.

## Break-glass console

When the control plane is down, the operator already reaches the machine some other way, over SSH or
a physical console. So break-glass is a Unix socket on the node, `korpis-agent console <workload>`,
authorized by a break-glass token or by being root on the machine, with every use journaled before
the session opens and delivered to the effect log on reconnect. No network surface is added. It does
not work from a phone: a mechanism that did would be a listening socket on every node, permanently,
for an event that happens twice a year.

**Observation frequency.** Every state transition is reported at once with `since`, the time the
node recorded it, so it is exact however late it travels. The node sends one heartbeat per interval
(30 seconds by default) carrying a digest of its workloads' observations; while it arrives and
matches, every unchanged observation is current without a write, and a workload becomes unknown when
its node misses three. A heartbeat per workload would cost a store write per workload per interval
for no information.
