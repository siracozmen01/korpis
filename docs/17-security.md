# Threat Model and Security Architecture

**Status:** design **Date:** 2026-08-07 **Depends on:** [`04-runtimes.md`](./04-runtimes.md),
[`08-identity.md`](./08-identity.md), [`02-architecture.md`](./02-architecture.md) **Resolves:**
open question 3 of `14-streams.md`; open question 5 of `15-observability.md` **Implements:**
Principles P3, P4; Rules K-1, K-2, K-3

---

## 1. The founding assumption

**Tenant code is hostile.** Not "may be untrusted", hostile, deliberately, by default, in every
document above this one.

This is not pessimism about customers. It is the accurate description of a platform whose entire
purpose is running software the operator did not write, chosen by someone the operator has never
met, frequently a modded game server assembled from plugins downloaded from a forum. The realistic
question is never whether tenant code will attempt to escape; it is whether it succeeds, and what
it reaches when it does.

Every panel in this market that has been broken was broken by getting this backwards, by treating
tenant input as data that needs checking rather than as an adversary that needs confining.

---

## 2. Adversaries, named

Threat models fail by being written about "attackers" in the abstract. These are the specific
parties whose interests diverge from the operator's, each of which forces a different mechanism:

| Adversary | Wants | Primary defence |
|---|---|---|
| **A tenant's workload** | escape isolation, read the host, reach other tenants | kernel confinement (§4), isolation tier (§5) |
| **A tenant's user** | reach objects outside their scope | grants, evaluated server-side on every call (§6 of `08-identity.md`) |
| **A sub-tenant** | escalate to their reseller's authority | attenuation, a grant cannot produce a stronger child (P6) |
| **A recipe author** | run code on nodes via a popular recipe | no arbitrary code in the install DSL (§4 of `09-recipes.md`), signing, digest pinning |
| **An extension author** | exceed granted authority | extensions are workloads with declared grants (§4 of `16-extensions.md`) |
| **An unauthenticated outsider** | reach the API, the agent, or a workload | no inbound agent ports (§3 of `02-architecture.md`); a workload is reachable only through endpoints it declares (§3 of `07-networking.md`); rate limits on every unauthenticated endpoint (§10 of `10-api.md`) |
| **A compromised node** | forge observations, reach other nodes, keep running after eviction | single-writer ownership, lease epochs, no node-held long-lived credentials (§7) |
| **A compromised control plane** | everything | acknowledged as total; blast radius and recovery in §9 |
| **Discord, or an external IdP** | none | trusted for exactly what it asserts (who someone is, which sets they belong to) and for nothing else; it holds no grants, and installing one requires authority over the whole organization it serves (§5.3 of `08-identity.md`) |
| **The operator** | none | **tenants are not protected from the operator.** Stated in §8, honestly. |

---

## 3. What the evidence says to build

The largest single class of published Wings advisories (§1.1 of `research/evidence.md`) is one
mistake: **a privileged process defending tenant-controlled paths by inspecting strings.** Symlinks,
hardlinks, TOCTOU races, `..` traversal, and mount tricks each beat string validation, and each did.
The second largest is a limit displayed and not held: disk quotas bypassed through SFTP streaming,
through an integer overflow in the quota arithmetic, and through a pre-flight check that went
stale. That class is why K-3 says a displayed limit must be enforced in the write path.

One more is worth naming because it is a different lesson. GHSA-pfvc-3p5h-x7h6 (Critical): tenant-
controlled egg template variables were rendered in a context where they could reach the daemon
token and registry credentials. The failure was not the templating engine. It was that the
credentials were reachable from the render context at all.

The third class, SSRF (three advisories), is a request the tenant chooses made from where the
tenant cannot stand: a remote file pull and a backup restore URL that reached the node's own
network. Korpis makes such requests wherever a recipe names a source, so each is bounded by
address, not by URL: a recipe's `fetch` reaches only the public internet and the networks the
operator names, checked at resolution and at every redirect (§4 of `09-recipes.md`).

Pulling a recipe or an image from a registry a recipe or a declaration names is bounded the same
way, by the control plane resolving a recipe and by the node fetching one. The registry client
takes no resolver of its own, so it is given a proxy instead: one on loopback, inside the same
process, that asks a credential made when it starts and applies the same policy to every
connection it makes, tunnelled HTTPS and forwarded plain HTTP alike. A token endpoint or a
redirect to a content delivery network that a registry names passes through it too, so a registry
cannot point the client anywhere the recipe could not. The registries the operator names as
development registries over plain HTTP are reached wherever they are; each process takes the same
`--fetch-allow` for a private mirror's network (§3 of `18-operations.md`). What it costs: every
registry connection passes through one more hop inside the process, and a registry on a private
network has to be named by the operator. The CLI's registry client is not bounded, because its
requests are its user's, made from its user's machine.

> Bounded 2026-10-15. A note here said registry pulls were not yet bounded; the change that
> bounded them is the one that removed it.

Two rules follow, and they are P3 and K-2:

> **Confine with the kernel, not with strings.** Validation is defence in depth; it is never the
> defence.
>
> **A secret that is not present cannot be leaked.** Reduce what is reachable before hardening the
> path to it.

---

## 4. Kernel confinement, mechanism by mechanism

Each of these stops a specific thing. Listing them without saying what they stop is how security
sections become decoration.

| Mechanism | Stops |
|---|---|
| user namespaces | root inside the workload being root on the host |
| mount namespaces | seeing, or reaching, any path outside the workload's tree |
| **Landlock** | the filesystem worker touching anything outside the tenant subtree, *including via symlink* |
| `openat2(RESOLVE_BENEATH)` | traversal and symlink escape at the syscall boundary, without a race window |
| seccomp | reaching syscalls the workload has no business calling, shrinking kernel attack surface |
| capability drop | every privileged operation not explicitly required |
| cgroups v2 | CPU, memory, PID, and I/O exhaustion (§10) |
| network namespaces + nftables | reaching other tenants, the host, or the metadata address (§6 of `07-networking.md`) |
| no-new-privs | setuid binaries inside the workload regaining privilege |
| read-only and `noexec` mounts | writing then executing in places that should hold only data |

**Path handling is the load-bearing case.** Korpis does not check a path and then open it, that is
the TOCTOU window that produced half the advisories. It opens with `RESOLVE_BENEATH` inside a mount
namespace, under a Landlock ruleset, from a process that has no access to anything else. If the
kernel resolves the path, it was legal; if it does not, no check was needed. The string is never
consulted as a security decision.

---

## 5. Isolation tiers, described honestly

K-10 makes isolation strength a per-workload operator choice. That choice is only meaningful if the
tiers are described truthfully, including what each one does **not** stop. P4 applies to security
claims more than anywhere else.

| Tier | Boundary | Stops | Does **not** stop |
|---|---|---|---|
| `process` | namespaces, cgroups, Landlock, seccomp, no container image | filesystem escape, resource exhaustion, network reach | kernel LPE, CPU side channels |
| `container` | the above, plus image isolation and a reduced syscall surface | the same, plus most host-filesystem exposure | **kernel LPE**, CPU side channels |
| `microvm` | a separate kernel (Firecracker) | kernel LPE, almost all escape classes, and the CPU side channels of the shared-kernel tiers **when its cores are dedicated** | hypervisor vulnerabilities, hardware-level channels, and cross-VM leaks between SMT siblings when cores are shared |
| `vm` | a separate kernel with full device emulation (QEMU/KVM) | as microvm | as microvm, with a larger device attack surface |

> Second external review, finding S46. Recorded in §17 of `23-walkthroughs.md`. The `microvm` row
> said it stops "most side channels", and §13 told customers who need side-channel isolation to
> pick `microvm`. A separate kernel does not stop the leaks that cross a shared physical core: the
> L1TF and MDS classes leak between SMT siblings whatever runs on them, which is why Firecracker's
> own production-host guidance disables SMT or keeps tenants off sibling threads. The row now says
> what the tier stops on its own and what it stops only with dedicated cores (§5.2 of
> `05-scheduling.md`), and §13 question 1 is resolved by offering exactly that.

**The `microvm` row as built** (2026-10-25, from the `firecracker` driver). Each guest's
Firecracker runs under Firecracker's jailer, in a chroot the driver builds again for every run,
holding hard links to the kernel and the drives and the workload's volumes bound in; dropped to the
first id of the workload's own range; in a pid namespace of its own; in a cgroup holding the
guest's limits; and in the workload's network namespace, whose tap is the guest's one interface (§6
of `04-runtimes.md`). Firecracker applies its own seccomp filters, which the driver never turns off
(§5 of `research/evidence.md`). The guest's kernel is Korpis's own build of a longterm release with
Firecracker's guest configuration (§5 of `21-stack.md`), booted with no PCI bus and its root the
read-only boot drive. What the row's "when its cores are dedicated" means as built (2026-10-25,
§5.2 of `05-scheduling.md`): a guest holding cores has a vCPU for each of their CPUs, each pinned
to its own, in a cgroup under the driver's `dedicated` one with their `cpuset`, and no other
tenant's guest or container runs on them; a guest holding none runs on cores it shares and does not
stop the leaks between SMT siblings the row names. Until 2026-10-25 a declaration asking for
dedicated cores was refused and every guest shared its cores (Finding 167 of `23-walkthroughs.md`).
The conformance suite's isolation cases, which would test this row with escape attempts, are held
for the maintainer (§8 of `04-runtimes.md`), so the row is argued from the implementation and not
yet tested against it.

**What a guest says to its host is read as data of fixed layout.** Added 2026-10-24, when the
driver first asked a running guest anything (§6 of `04-runtimes.md`). The `firecracker` driver is
root, and a guest is the tenant's kernel, so the channels between them are as narrow as the work
allows: the run's record on the config drive is sixteen bytes, and the guest's agent on the vsock
control port answers in frames whose kind and length are checked before anything else is read and
whose answers are numbers of fixed layout; nothing a guest writes is parsed as a structure by the
host. The host listens on two vsock ports of a guest whose workload has an identity directory, and
on none of any other: what a guest opens on them is relayed, byte for byte and unread, to that
workload's own identity socket and API relay on the host (§6 of `04-runtimes.md`), which are what a
container reaches through its mount, so a guest reaches nothing on its host that its workload could
not reach as a container. The sockets Firecracker hands those connections to are its own user's
alone, made where the guest's Firecracker cannot write and moved into its chroot, so another local
user cannot borrow a workload's identity through them. (Corrected 2026-10-24: this said the host
listened on no vsock port, which was true until the relays of a guest's `/run/korpis` were built.)
What it does not stop: a bug in Firecracker's vsock device is a hypervisor vulnerability, in the
row above; and the agent answers for the workload, which is root of its guest and can answer in its
place, so its answer is evidence of liveness and of nothing else. A file session in a guest (§4.4
of `04-runtimes.md`) is one of the two places the node decodes what a guest wrote as a structure
(corrected 2026-10-24: this said the one place, until installs ran in a guest): each frame the
guest's worker answers is a `FileResponse`, bounded in length before it is read and decoded by the
same memory-safe decoder that reads a host worker's, and passed on to the client. A workload that
is root of its guest can answer in its worker's place, so what a session in a guest says of its
files is the workload's word for them, which is all a session's answer ever was. A stopped
microVM's session is served by a service guest that runs nothing of the workload's, booted from
Korpis's own boot drive with no image; its kernel still parses a filesystem the tenant formatted,
which is the point of putting that parse behind the hypervisor, so a volume crafted to attack a
guest kernel reaches a guest that holds only that tenant's own volumes. A block device's backup is
browsed the same way (§5.4 of `06-storage.md`): a service guest of its own holds a copy of that
tenant's image restored for the session and nothing else, and its sessions are file sessions,
decoded as above. An install there (§4 of
`09-recipes.md`) is the other place the node decodes a structure from a guest: the install job's
`InstallResult`, bounded by the agent's frame and decoded as a host install's is, and its progress,
a number of fixed layout; what the install reads reaches the guest on a drive the host made from
its own store, holding only what that install names.

**A device given to a workload is its host driver given to the tenant.** Designed 2026-10-25 (§6 of
`05-scheduling.md`). A device in `passthrough` or `timeslice` reaches a `container` as a node of
the host's own kernel driver for it, so a vulnerability in that driver is a kernel vulnerability
the tenant can reach, in the row of the tier above; GPU drivers are large and parse what a workload
sends them. A device's Container Device Interface file is host configuration, and its hooks run as
root on the host when each container holding it is created: they are as trusted as the vendor's
driver the host installed, which is why the agent and the driver read only files and hook programs
that root alone can write (§6.5 there), and why the driver reads a device's edits itself rather
than taking them from the agent, which could otherwise mount any host path or run any program as
root through a device it named (Finding 169 of `23-walkthroughs.md`). NVIDIA's container toolkit
had a container escape in its default mode, which its CDI mode was reported not to share (§5 of
`research/evidence.md`), and a toolkit's hook is still code of the vendor's run as root. What it
does not stop: a tenant holding a device can attack its driver; a compromised agent can give a
device one workload holds to another of its node's (§6.1 of `02-architecture.md`); holders of one
device in `timeslice` share its memory and can starve or observe one another as far as the device
lets them; and a `microvm` is given no device, so the tier that would put the device's driver
behind a hypervisor offers none, until a `vm` driver gives one through VFIO.

**A shared-kernel tier does not survive a kernel privilege-escalation vulnerability.** That is true
of every container platform, it is true here, and it is why the tier is an operator choice rather
than a platform property. An operator running their own services picks `process` and gets density;
an operator running arbitrary customer code picks `microvm` and pays for it in memory and boot
time. There is no configuration that gives both, and Korpis does not imply one exists.

§2 of `04-runtimes.md` explicitly refuses to ship an unhardened container tier, because a tier that
looks like isolation and is not is worse than a tier honestly labelled `process`.

---

## 6. Privilege separation inside the agent

The agent is where tenant data meets root, so it is decomposed until no single process holds both
(§6 of `02-architecture.md`):

```
supervisor          unprivileged. talks to the control plane, holds no root, touches no tenant path.
privileged helper   root, tiny, no tenant input. performs an enumerated set of operations.
filesystem workers  one per workload, in a namespace holding only its volumes, Landlocked to them.
backup jobs         one per backup or restore, started by the helper, holding one workload's data
                    key and only the capabilities their work needs, Landlocked to one directory.
install jobs        one per install, started by the helper, running as the workload's own ids in a
                    user namespace whose root holds only that workload's volumes (09 §4).
runtime drivers     separate processes, each holding only its own driver's privileges.
image builds        one per image a microvm node has not built, started by its driver, applying
                    a publisher's layers as the workload's ids in a user namespace (04 §6).
ingress proxy       one per node, unprivileged and confined, the only process parsing requests
                    from the internet; holds its routes and certificates in memory (07 §3.2).
```

The argument is a privilege union. A single process performing all four roles holds the union of
their privileges at all times, so any bug anywhere in it is a bug in a root process with access to
every tenant's data. Split, a bug in the filesystem worker is a bug confined to one tenant's
subtree by the kernel, and a bug in the supervisor is a bug in an unprivileged process.

Drivers are separate for the same reason (§3 of `04-runtimes.md`): the QEMU driver needs privileges
the OCI driver does not, and a merged runtime would grant both to both.

**What the split does not do, stated as plainly as what it does.**

> Second external review, finding S54. Recorded in §17 of `23-walkthroughs.md`.

- **The `oci` driver is root-equivalent.** It talks to containerd, and anything that can talk to
  containerd can ask for a privileged container with the host's root filesystem mounted. The
  supervisor holds no privilege, but it can ask the driver for things, so the property that
  actually holds is narrower than "a supervisor bug is a bug in an unprivileged process": a
  compromised supervisor can obtain only what the drivers will accept, and the drivers accept only
  specs inside their tier's invariants (§6.1 of `02-architecture.md`). The driver's validation is
  therefore part of the security boundary, and is reviewed as such.
- **containerd and runc are in the trusted base of the `container` tier.** A vulnerability in either
  is a vulnerability in the tier, and the tier's row in §5 is only as strong as they are.
- **The helper is root.** It is tiny and takes no tenant input, which makes bugs in it unlikely, not
  impossible; a bug in it is root on the node.
- **A backup job reads tenant data with privilege.** It must read every file of a volume whoever
  owns it, or restore every owner, so it keeps `CAP_DAC_READ_SEARCH` to back up, and
  `CAP_CHOWN`, `CAP_FOWNER`, `CAP_DAC_OVERRIDE`, `CAP_FSETID`, `CAP_MKNOD`, and `CAP_SETFCAP` to
  restore (§5.5 of `06-storage.md`). Landlock confines it to the one directory and to connections
  to the store's port, and its walk never follows a link, so a bug in it reaches that directory and
  that store; but it is uid 0 in the host's namespaces, and a kernel without Landlock (before 5.13,
  or before 6.7 for the port rule) leaves it confined by its own code alone, which its result says.
  Name resolution is not confined, since it travels over UDP, which Landlock does not govern.

- **The ingress proxy holds the private keys of the endpoints it serves.** It must, to terminate
  their TLS (§3.2 of `07-networking.md`). It is the process most exposed to the internet, which is
  why it is a process of its own, holding nothing else: a bug in it reaches those keys and the
  traffic through it, and not the agent's session, the helper, or any workload's data. On a member
  of an edge group it serves every name the group serves, so a bug in it there reaches those
  keys too; and its node lets it open TCP connections through the mesh as well as through a
  workload's link, to any address the mesh routes, of which each node admits from it only the
  ports of the routes its group serves there (§3.1 of `07-networking.md`). Over HTTP/3 it parses
  QUIC as well, in user space, from any client that reaches UDP port 443 (built 2026-10-27, §3.2
  of `07-networking.md`): that widens what can reach a bug in it, not what such a bug reaches, as
  its sockets are handed to it and it opens no new kind of connection.

- **A client certificate the ingress accepts is not checked for revocation** (built
  2026-10-22, §3.2 of `07-networking.md`). The proxy verifies it against the authorities the
  endpoint names and passes it on, but consults no CRL and no OCSP responder, so a certificate its
  authority revoked is accepted until it expires. An endpoint that must refuse one sooner issues
  short-lived certificates, or checks the `Client-Cert` header itself, which the proxy writes and
  no client can. And the header is only as good as the path to the workload: a workload reached
  other than through the proxy, by a port it also exposes, reads whatever a client sends there.

None of this changes the design. It changes what the design is allowed to claim, which is the
point of this section.

---

## 7. Secrets

The principle from §3: reduce reachability first.

- **Nodes hold no long-lived registry credentials.** §7 of `02-architecture.md` issues short-lived,
  digest-scoped pull tokens instead. A compromised node yields a token that expires and that can
  pull one thing.
- **Nodes hold no standing object-storage credentials and no repository keys.** For each backup or
  offload job a node receives the data key of the one workload concerned and a short-lived
  credential that can write, read, and list objects under that workload's prefix, delete none, and
  reach nothing else; a store that cannot issue such a credential is served by its provider, which
  issues one of its own (§5.5 of `06-storage.md`; corrected 2026-10-15, when it said such a target
  was written through a relay holding the store's credential). The first version said nodes held no
  object-storage credentials while the backup flow streamed encrypted chunks from the node straight
  to the repository, which required both a key and a credential on the node (finding S37). Now the
  node holds both, for one workload at a time, for the length of one job.
- **Tenant secrets never enter a template context.** §6 of `09-recipes.md` renders from a flat,
  pre-resolved, enumerated variable map in a logic-less language, inside the workload's own
  filesystem worker, and secret fields are delivered only through channels the tenant cannot read
  back (§5.2 of `09-recipes.md`).
  There is no ambient scope to walk, which is what defeats the GHSA-pfvc-3p5h-x7h6 class rather
  than escaping harder.
- **Secrets are redacted before persistence**, on the node, in the write path (§7 of
  `14-streams.md`), and that redaction is labelled best-effort in the interface, because pattern
  matching cannot catch a workload that encodes its own secret first.
- **Capability tokens are short-lived**, travel in the URL fragment when shared (§6.1 of
  `08-identity.md`), and never reach an extension's frame (§5 of `16-extensions.md`).
- **Nodes hold the TLS keys of the ingress endpoints placed on them, and no others.** A node
  obtains an endpoint's certificate and key only while the endpoint's workload is placed on it, and
  keeps them in its state directory for a restart without the control plane; the control plane
  keeps every key sealed by its TLS key (§3.2 of `07-networking.md`).
- **Nodes hold a block volume's credential only while its workload is placed on them**, scoped to
  that volume where the storage can scope one; the provider holds it for the volume's life (§2.1
  of `06-storage.md`; designed 2026-10-27). A shared class's mount is the operator's, and Korpis
  holds no credential of it.
- **The agent's own key** is per-node, rotatable, and its compromise is bounded by §9.

---

## 8. What the operator can see, stated plainly

Tenants are not protected from the operator. The operator controls the hardware, the kernel, the
storage, and the network; any claim otherwise would be false, and P4 forbids it.

What Korpis provides instead is **visibility**, which is achievable and is what actually changes
behaviour:

- Operator access to a tenant's console, files, or backups produces an `Effect` naming the
  operator, the action, and the object (§4 of `15-observability.md`). As built, 2026-10-21: a
  read of a console is journaled by the node that serves it, and a read of its offloaded segments
  recorded by the control plane, each as the reader's act (§8 and §4 of `14-streams.md`); until
  then a read on the node was recorded nowhere (Finding 122 of `23-walkthroughs.md`). A node its
  operator has changed can serve a read and journal nothing: the effect makes the supported path
  visible, not the unsupported one.
- **That effect is visible to the tenant** (§7 of `08-identity.md`), in their own audit view.
- Break-glass access is the same mechanism, not a bypass, a grant, issued, time-bounded, logged (§6
  of `08-identity.md`).
- **Relays see the sessions they carry.** TLS terminates at the relay (§3.1 of
  `02-architecture.md`), so console output, keystrokes, and files in transit are plaintext there.
  Relays are operator infrastructure, and this is one more thing the operator can see, stated here
  rather than left to be inferred from a diagram. A relay also holds the route key that tells it
  which node each session goes to (§3.1 of `02-architecture.md`); a stolen route key lets whoever
  also holds a client's routes learn which node its workloads run on, which a tenant is otherwise
  never told.
- **A relay that serves SFTP can open a tenant's files as the tenant.** It checks that an SFTP
  client holds the SSH key it offers, which no library lets it prove to the control plane, and
  the control plane mints a file token for whoever registered that key on the relay's word (§3.1
  of `02-architecture.md`, §9.9 of `08-identity.md`). A compromised relay serving SFTP can
  therefore read and write the files of anyone with a registered key, on any workload their
  grants reach, without that person connecting; it gains no console, no API call, and no other
  action. Each token it asks for is a `workload.file.sftp_authorized` effect naming the relay on
  the tenant's own trail, and a relay that does not serve SFTP gains nothing, which is why it is
  off until an operator turns it on.
- **A host reads a tenant's private data only through a grant that names the tenant**, which the
  tenant sees issued and sees used (§3.3 of `08-identity.md`).
- **Tenant key custody limits backups, not the operator.** Under `tenant` custody (§5.5 of
  `06-storage.md`) a data key is opened only by the tenant's own key management service, each
  time, and the tenant can end every opening by revoking one key. It does not stop the operator
  reading a backup while its job runs on the operator's node, nor using the credential the
  key management extension holds, since that extension runs on the operator's nodes too; every
  such use is an unwrap in the tenant's own log, which is what the custody buys.

> **Resolving open question 5 of `15-observability.md`**, which accesses are exempt from tenant
> visibility: **none are.** An operator debugging a node-wide failure touches many tenants and
> produces many effects; that is correct and is not a reason for an exemption. Exemptions are how
> audit logs become decorative, and an operator who genuinely needs unobserved access has root on
> the machine and does not need Korpis to provide a supported path to it. Volume is a UI problem,
> solved by grouping effects under the operation that caused them, not a reason to hide them.

> **Resolving open question 3 of `14-streams.md`**, tenant-chosen short retention versus operator
> need for evidence: the operator may set a **retention floor**, a minimum below which a tenant
> cannot go, and that floor is **disclosed to the tenant** in the interface alongside the retention
> setting. An operator cannot silently retain more than they declared. This is the same shape as
> `min_retention` on backup retention policies (§5.3 of `06-storage.md`), and the disclosure is the
> part that makes it legitimate rather than surveillance.
>
> **As built, 2026-10-14:** `korpis-server serve --stream-retention-floor-days N`, none by default,
> keeps every offloaded console segment at least N days whatever its workload declares, and
> `StreamService.GetStream` shows every reader of a stream the floor beside the retention it
> results in (§4 of `14-streams.md`). It applies to offloaded segments, not to what a node holds:
> raising a node's retention would enlarge the tenant's quota for the operator's purpose.

---

## 9. Compromise, and what it costs

**A compromised node.** The blast radius is that node's tenants, including the backups of the
workloads placed on it, which the node could already read live, plus whatever its credentials
reached, which §7 has deliberately made small: no other workload's backups, no other node's
assignments, no registry beyond one artifact for a few minutes. It can also obtain the credentials
of the workloads placed on it, which act as those workloads (§9.5 of `08-identity.md`): each lasts
fifteen minutes at most, ends at once when its workload is placed elsewhere, and none can be had
for a workload placed elsewhere. Recovery: revoke the node key, and **increment the lease epoch**.
§4.5 of `02-architecture.md` and §8 of `03-state.md` make epochs monotonic and fencing mandatory,
so a compromised agent that keeps running cannot reacquire ownership of anything, and cannot write
an observation the control plane will accept. Workloads are rescheduled elsewhere from their
intents, which are unaffected. The node is not trusted to clean itself up; it is fenced and
rebuilt. As built (2026-10-16), the recovery is one call, removing the node as compromised (§3 of
`18-operations.md`), which decommissions it whatever it holds: its session ends and its key opens
no other, each workload placed on it is placed again from its target on the nodes that remain, and
made there by a `remake` plan if it was made on the node (§2.4 of `05-scheduling.md`, Finding 204
of `23-walkthroughs.md`), and its endpoints there are dropped. Its leases are fenced by the clocks
of §4.5 of `02-architecture.md`, not by a message it would have to accept: it cannot renew them, so
each is over within one term, and the next issue, to another node, raises the epoch, so
incrementing the epoch is that issue rather than a step of its own. What it does not do: its
volumes are lost with it, since nothing moves a volume off a node before migration; a workload
attached to one stays unplaced, saying why, until it is restored from a backup elsewhere. On one
machine there is no other node, so every workload stays unplaced until the machine is rebuilt and
enrolled again.

Designed 2026-10-27, with storage over the network and replication (§2.1 and §2.2 of
`06-storage.md`): a workload whose volumes are in a shared or block class is placed again with its
data, and a replicated one from its freshest replica, losing what that replica lacks, as failover
places them (§2.4 of `05-scheduling.md`); a block class's provider refuses the removed node the
device at the new attach. The node's blast radius grows by what it held for others: the replicas it
kept of volumes running elsewhere, which it could read as it reads its own, and the credentials of
the block volumes of the workloads placed on it, each scoped to its volume where the storage can
scope one. A replica it received cannot have been written by it into another node's volume: a
transfer runs from a volume's own node to its replicas, on a ticket naming both, and the mesh
admits it from the sender's key alone. A node of another organization reaches no store an
organization's node declared first, however it names its own mount, while that organization holds a
live volume in it or a node offering it, and a report it makes keeps or deletes only its own
workloads' volumes (Findings 201 and 203 of `23-walkthroughs.md`). What a shared class does not
stop: a client of its export that writes as root, which can replace a volume's directory with a
link between a driver's check of where it resolves and its mount, and so is trusted as a node is;
the helper itself makes and gives away what it makes there only through descriptors, never through
a name such a client could replace (Finding 202), and an operator is told to export the store to
the installation's nodes alone (§3 of `18-operations.md`).

It also held the private keys of the ingress endpoints placed on it, which outlive the fence: a key
keeps working for as long as its certificate is valid. So removing a node as compromised revokes,
through the ACME directory, every `acme` certificate whose key it was given, and issues each
endpoint a new one with a new key; a `provided` certificate cannot be revoked by Korpis, and its
endpoint's status says its key was exposed, for its organization to replace. Added 2026-10-15, with
the ingress design (§3.2 of `07-networking.md`). **As built (2026-10-16), and what evaluates it:**
the control plane, from its own record of every certificate key it gave a node, written when it
answers the node's request: the certificate, the revision, the key's identifier (a digest of the
leaf's public key), and the chain given. Removing the node as compromised marks each certificate
whose current key it was given exposed and tells the certificate's organization in a
`certificate.exposed` event, critical; an `acme` one is given a new key at once, on whatever node
its endpoint is placed next; every chain of an `acme` certificate the node was given, each
revision, is revoked through the directory with the reason `keyCompromise`, asked again until the
directory answers. A `provided` one stays exposed, in its status, until its organization replaces
it with a chain for another key; a new chain for the same key is still exposed. What revocation
does not do: reach a client that does not check, and many do not, so the protection that holds
regardless is a certificate's short life; revocation shortens the window for the clients that look.

**A compromised edge.** Designed 2026-10-26, with edges (§3.1 of `07-networking.md`). A member of
an edge group sees every connection to the addresses it holds as any router does, and its
ingress proxy terminates every name the group serves, so its blast radius is the reachability and
the plaintext of every workload behind the group, which is its organization's and those below it,
and the keys of those names' certificates. Into a workload's node it reaches less: the node takes
from its GRE link only connections to the ports its group forwards to that node's workloads, from
whatever source address it claims, which is what a client of those ports could send anyway, and
from the mesh, from the member's own mesh address, only connections to the ports of the `ingress`
endpoints its group serves there, which its proxy would carry anyway (built 2026-10-27). A
probe does not catch it, since a member that forwards the probe and drops the rest passes. The
recovery is a node's: removing it as compromised ends its session, its lease on each address runs
out within the group's term and the next member's raises the epoch, and the certificates whose
keys it was given are revoked and renewed as above. What it does not reach: workloads of an
organization its group does not serve, since a group is its organization's own nodes (§3.1 there).

### 9.1 Egress is enforced by the party being contained

> Resolves open question 6 of `07-networking.md`; Finding 8 of `23-walkthroughs.md`.

Egress rules are applied at the source node by that node's agent, and a compromised agent can
ignore them, which puts a control in the hands of the adversary it is meant to constrain.

The honest framing first: **an attacker who has compromised the agent as far as its root
components (the helper, or a root-equivalent driver, §6) has root on that machine.** A compromised
supervisor alone does not, which is what §6 exists to ensure, but egress is not the place to rely
on that distinction. At that point the
attacker can already rewrite nftables, read every tenant's data on the node, and forge
observations. Egress is the least of what has been lost, and containing the outbound traffic of a
machine whose root you no longer hold is defending the wrong thing. The answer to node compromise
is the paragraph above (revoke, fence, rebuild) not a cleverer firewall.

That said, one real improvement was being left on the table:

**East-west egress is enforced at both ends.** A workload on a compromised node reaching a workload
on a healthy node is stopped by the *destination's* agent, which already maintains a network
namespace and nftables rules for its own workloads. The source enforces for efficiency (drop early)
and the destination enforces because it does not trust the source. Genuine defence in depth at
essentially no cost, and it means a compromised node cannot use the fleet as its lateral path.
Across nodes this is the mesh's rule (§9.1 of `07-networking.md`, designed 2026-10-25): WireGuard
accepts a packet from a peer only with a source in that peer's own pool, so a compromised node
cannot speak as another, and the destination admits from the mesh only the dependents of its
workloads, to their dependencies' ports. The mesh's UDP port answers nothing that does not carry a
peer's key.

> Built 2026-10-25 (`korpis-helper`'s `overlay`): the destination's node drops, and counts under
> the comment `overlay`, everything arriving on `korpis-overlay` that is neither a reply nor an
> admitted dependent's connection to its dependency's port, whether bound for a workload or for
> the node itself. What it does not stop: the admission names addresses, and a compromised node
> can send from any address of its own pool, so it reaches whatever any dependent on it is
> admitted to, which it could reach through that dependent anyway, and nothing else.
>
> Corrected 2026-10-26 (Finding 178 of `23-walkthroughs.md`): a node's pools were taken as it
> reported them, so a compromised node could have had every other node route a prefix of its
> choosing, the operator's own LAN among them, to itself; the mesh now routes to a node only the
> pool the control plane gave it and the prefix its identity derives. And every node was told of
> every other: two nodes are now paired only where one's organization is the other's or above
> it, so a tenant's node learns nothing of a sibling tenant's. What a node still learns, of each
> node it is paired with, is its public key, the address it advertises, which its clients are
> told anyway, and its pools.
>
> Corrected again 2026-10-26 (Finding 180 of `23-walkthroughs.md`, found designing rules that name
> workloads). The destination enforced at the mesh and not at the node's other doors: a
> connection from outside the node, which a routed IPv6 prefix makes the whole internet and a
> network routing a node's IPv4 pool to it makes that network, reached every declared port of a
> workload whatever its exposure. An `overlay_only` endpoint, which other workloads alone reach,
> was reached by anyone who could route to it, and an `ingress` endpoint directly, past its
> proxy's TLS, client certificates, and limits. The node now lets in from outside only what it
> translated from a port of its own and the ports of endpoints exposed at the workload's own
> address, `node_port`, `node_bound`, and `dedicated_ip`, and drops and counts the rest under
> `exposure` (§4 of `07-networking.md`).

**North-south egress is uncontained on a compromised node, and this is stated rather than
implied.** Only the source, or a chokepoint the operator owns, can enforce it, and nothing at this
layer does better.

**So Korpis publishes each node's expected egress profile** in a machine-readable form. An operator
who needs enforcement at a layer the node does not control (a VLAN, a security group, an upstream
ACL) configures it from that profile instead of guessing. Korpis does not operate that layer and
does not pretend to; it supplies the only thing it credibly can, which is an accurate statement of
what legitimate traffic looks like.

**A compromised control plane.** Total. There is no partial story here and inventing one would be
dishonest: the control plane holds the authority to issue grants. What can be bounded is
reconstruction, §8 of `03-state.md` requires restore to fence with a monotonic `max_issued_epoch`
watermark and to rotate the signing key, so a restored control plane can neither issue an epoch a
live agent would accept as current nor honour a token it has no record of, which is what prevents a
compromise or a restore from turning into two authorities.

**A compromised tenant account.** Bounded by that subject's grants, all of which are enumerable,
all of which are revocable individually, and every use of which is in the effect log. There is no
admin flag to inherit.

---

## 10. Resource exhaustion, indexed

Every resource a tenant can exhaust, and where it is bounded. This list exists to be checked; an
unbounded resource is a denial-of-service vulnerability with a friendlier name.

| Resource | Bound | Where |
|---|---|---|
| CPU | cgroup v2 weight and quota | §5.2 of `05-scheduling.md` |
| memory | cgroup v2 `memory.max`, no overcommit on reservations | §5.2 of `05-scheduling.md` |
| processes and threads | cgroup `pids.max`; this is the fork bomb | §5.2 of `05-scheduling.md` |
| disk bytes | filesystem quota in the write path, returning `EDQUOT` | §3 of `06-storage.md` |
| **inodes** | inode quota, a million empty files fills a filesystem with bytes to spare | §3 of `06-storage.md` |
| disk I/O | cgroup v2 `io.max` ceilings, and proportional shares under `io.cost`, where the storage class declares them enforceable (ZFS writes are not) | §3 of `06-storage.md` |
| backup I/O | the backup process runs in its own throttled cgroup | §3 of `06-storage.md` |
| network bandwidth | shaping per endpoint | §7 of `07-networking.md` |
| **conntrack entries** | per-workload limit, a shared table is a shared failure | §6 of `07-networking.md` |
| ports and addresses | allocation is explicit | §3 of `07-networking.md` |
| log volume | stream rate limit, drop with gap marker, never block | §5 of `14-streams.md` |
| log storage | counts against the tenant's storage quota | §4 of `14-streams.md` |
| metric cardinality | tenant-controlled values are never labels | §3 of `15-observability.md` |
| API calls | rate limits per authenticated subject and per organization, never multiplied by attenuation | §10 of `10-api.md` |
| unauthenticated attempts | per source, answered `RATE_LIMITED` past the source's budget, a working credential still admitted; a guessable secret (a password, a second factor) also per account, sparing browsers that have signed in to it (corrected 2026-10-16, Finding 63) | §10 of `10-api.md` |
| recorded denials | charged to the caller and coalesced per window | §10 of `10-api.md` |
| node journal | a declared size, coalescing, then a gap record | §4.10 of `02-architecture.md` |
| control-plane work | expensive operations carry separate budgets | §10 of `10-api.md` |
| **outbound abuse** | internal destinations denied by default, public internet rate-limited, SMTP denied, detection | §6 and §8 of `07-networking.md` |

---

## 11. Supply chain

Four things arrive from outside and each is pinned by digest and verified before use:

| | Verification |
|---|---|
| recipes | cosign signature, decided on the digest under the organization's trust policy (§7 of `09-recipes.md`), digest-pinned, `fetch` without a hash is a **parse error** (§4 of `09-recipes.md`) |
| extensions | cosign signature, digest-pinned, declared grants approved at install |
| container images | digest-pinned; tags are resolved once and recorded, never re-resolved silently |
| the Korpis binaries themselves | reproducible builds, signed releases, published SBOM |

Imported Pterodactyl eggs are the deliberately dangerous case, and §9 of `09-recipes.md` handles it
by grading rather than refusing: `clean`, `hashable`, and `unverified`, where `unverified` runs in
a microVM with no network beyond confirmed hosts. An import path that pretends imported artifacts
are trustworthy would undo this entire document.

---

## 12. Disclosure

A published security policy, a contact, and a stated response window exist before the first
release, not because it is best practice, but because K-8 makes governance a day-one decision and a
project without a disclosure path receives its vulnerabilities as public tweets.

Advisories are published with the same detail Korpis' own research used to derive these rules
(`research/evidence.md`): what was reachable, what it affected, which versions, and what the fix
changed. A project that learned this much from reading other people's advisories does not get to
publish vague ones.

---

## 13. Open questions

1. **CPU side channels between tenants in shared tiers. Resolved here: dedicated cores are offered,
   and they are what defends against them** (built in Phase 3, 2026-10-25, and refused at
   declaration until then, corrected 2026-10-25 by Finding 167 of `23-walkthroughs.md`). Not
   defended against by default; §5 says so plainly. Whether Korpis offers core pinning and
   sibling-thread exclusion as a declared workload property (with the density cost stated) was
   unresolved, and the default advice was to pick `microvm`, which on its own does not stop the
   sibling leaks (finding S46).

   `cpu.isolation: dedicated_cores` (§5.2 of `05-scheduling.md`) pins a workload to whole physical
   cores and keeps every other tenant off their SMT siblings, and by default the host's own
   processes too, each holding workload's cgroup a cpuset partition of its own; a host that keeps
   its processes on every CPU leaves its agent and services exposed on a held core's sibling to
   the leak classes the cores defend against, and its drivers' declarations say so. It is honest
   on both counts: it is the only configuration that defends against the cross-core leak classes,
   in any tier, and it costs the whole core, which the scheduler accounts and the meter bills.
   Combined with `microvm` it is the strongest isolation Korpis offers; neither alone is. What is
   tested of it on the machine this project is built on is the host sharing the cores: other
   tenants kept off, and no partition made (§8 of `04-runtimes.md`).

   > Corrected 2026-10-25: the reflow that added the host's half above, in the commit that
   > designed dedicated cores, ran questions 2 to 5 into this paragraph, so the list read as one
   > question. They are restored as they stood.
2. **Hardware attestation.** Verifying that a node runs the kernel and agent it claims would raise
   compromised-node detection substantially and drags in TPM, measured boot, and a key-management
   story disproportionate to a single-operator install. → `18-operations.md`
3. **Confidential computing tiers.** AMD SEV-SNP and Intel TDX would let a tenant be protected
   *from the operator*, which §8 currently states is impossible. That is a real fifth tier and a
   large commitment. → `04-runtimes.md`
4. **Recipe author reputation. Resolved here: operator-managed allow-lists, nothing central.**
   Signing proves who published a recipe, not whether they should be trusted. Anything resembling
   curation collides with "Korpis is not a marketplace" (§2 of `00-overview.md`). It was routed to
   `09-recipes.md`, which never carried it (finding S18). The mechanism already exists in two
   places and is now named as the answer: `TrustPolicy.accepted_signers` per organization (§7 of
   `09-recipes.md`) and the publisher or registry allow-list for extensions (§4.1 of
   `16-extensions.md`). Korpis supplies verification and policy; reputation is the operator's.
5. **Windows isolation parity.** K-9 requires the runtime interface to express Windows on day one;
   job objects and server silos are genuinely weaker than Linux namespaces, and §5's tier table has
   no honest Windows column yet. It needs one before Windows ships, not after. → `04-runtimes.md`
