# Security

The threat model: who the adversaries are, how the kernel confines them, what each isolation tier
does and does not stop, how privilege is split on a node, where secrets are, what the operator can
see, what a compromise costs, and every resource a tenant can exhaust.

## Contents

- [Tenant code is hostile](#tenant-code-is-hostile)
- [Adversaries](#adversaries)
- [What the evidence says to build](#what-the-evidence-says-to-build)
- [Kernel confinement](#kernel-confinement)
- [Isolation tiers, described honestly](#isolation-tiers-described-honestly)
- [Privilege separation on a node](#privilege-separation-on-a-node)
- [Secrets](#secrets)
- [What the operator can see](#what-the-operator-can-see)
- [Compromise, and what it costs](#compromise-and-what-it-costs)
- [Egress is enforced by the party contained](#egress-is-enforced-by-the-party-contained)
- [Resource exhaustion, indexed](#resource-exhaustion-indexed)
- [Supply chain and disclosure](#supply-chain-and-disclosure)

## Tenant code is hostile

Not "may be untrusted": hostile, deliberately, by default. A platform for running software the
operator did not write, chosen by someone the operator never met, often a modded game server
assembled from plugins downloaded from a forum, has to ask not whether tenant code will attempt to
escape but whether it succeeds and what it reaches. Every panel in this market that was broken was
broken by treating tenant input as data needing checks rather than an adversary needing confinement.

## Adversaries

| Adversary | Wants | Primary defence |
|---|---|---|
| a tenant's workload | escape, read the host, reach other tenants | kernel confinement, the isolation tier |
| a tenant's user | reach objects outside their scope | grants, evaluated server-side on every call |
| a sub-tenant | escalate to its reseller's authority | attenuation: a grant cannot yield a stronger child |
| a recipe author | run code on nodes through a popular recipe | no code in installs, signing, digest pinning |
| an extension author | exceed granted authority | extensions are workloads with declared grants |
| an unauthenticated outsider | reach the API, an agent or a workload | no inbound agent ports; only declared endpoints; rate limits |
| a compromised node | forge observations, reach other nodes, outlive eviction | single-writer ownership, lease epochs, no long-lived node credentials |
| a compromised control plane | everything | acknowledged as total; bounded reconstruction |
| an external identity provider | | trusted for exactly what it asserts; holds no grants |
| the operator | | tenants are not protected from the operator, stated |

## What the evidence says to build

The largest class of published Wings advisories is one mistake: **a privileged process defending
tenant-controlled paths by inspecting strings.** Symlinks, hard links, TOCTOU races, `..` traversal
and mount tricks each beat string validation, and each did. The second is a limit displayed and not
held: disk quotas bypassed through SFTP streaming, an integer overflow in quota arithmetic, a stale
pre-flight check. A third, GHSA-pfvc-3p5h-x7h6, rendered tenant-controlled template variables where
they could reach the daemon token and registry credentials: the failure was not the template engine
but that the credentials were reachable at all. The fourth, SSRF (three advisories), is a request
the tenant chooses made from where it cannot stand: a remote file pull and a backup restore URL
reaching the node's own network.

So Korpis bounds such requests by address, not by URL: a recipe's `fetch` reaches only the public
internet and networks the operator names, checked at resolution and every redirect
([recipes](recipes.md#fetching)). Registry pulls are bounded the same way, the registry client given
an in-process loopback proxy with its own credential that applies the policy to every connection,
tunnelled or plain, so a token endpoint or CDN redirect a registry names cannot point the client
anywhere a recipe could not. The CLI's registry client is not bounded, its requests being its user's
from its user's machine.

> **Confine with the kernel, not with strings.** Validation is defence in depth, never the defence.
>
> **A secret that is not present cannot be leaked.** Reduce what is reachable before hardening the path.

## Kernel confinement

| Mechanism | Stops |
|---|---|
| user namespaces | root inside a workload being root on the host |
| mount namespaces | seeing or reaching any path outside the workload's tree |
| Landlock | a worker touching anything outside the tenant subtree, links included |
| `openat2(RESOLVE_BENEATH)` | traversal and link escape at the syscall boundary, with no race window |
| seccomp | syscalls a workload has no business calling |
| capability drop | every privileged operation not explicitly required |
| cgroups v2 | CPU, memory, PID and I/O exhaustion |
| network namespaces and nftables | reaching other tenants, the host, or the metadata address |
| no-new-privileges | setuid binaries regaining privilege |
| read-only and `noexec` mounts | writing then executing where only data belongs |

**Path handling is the load-bearing case.** Korpis never checks a path and then opens it, the TOCTOU
window behind half the advisories. It opens with `RESOLVE_BENEATH` inside a mount namespace, under
Landlock, from a process with access to nothing else: if the kernel resolves the path it was legal,
and if not, no check was needed. The string is never a security decision.

## Isolation tiers, described honestly

| Tier | Stops | Does not stop |
|---|---|---|
| `process` | filesystem escape, resource exhaustion, network reach | kernel privilege escalation, CPU side channels |
| `container` | the same, and most host-filesystem exposure | kernel privilege escalation, CPU side channels |
| `microvm` | kernel privilege escalation, almost every escape class, and the shared-kernel side channels when its cores are dedicated | hypervisor vulnerabilities, hardware channels, and leaks between SMT siblings when cores are shared |
| `vm` | as `microvm` | as `microvm`, with a larger device surface |

A separate kernel does not stop leaks across a shared physical core: the L1TF and MDS classes leak
between SMT siblings whatever runs on them, which is why Firecracker's production guidance disables
SMT or keeps tenants off sibling threads. **Dedicated cores**
([resources](resources.md#dedicated-cores)) are the only configuration defending against cross-core
leaks in any tier, at the cost of the whole core, accounted and billed; with `microvm` they are the
strongest isolation Korpis offers, and neither alone is. A host that keeps its own processes on
every CPU leaves its agent and services exposed on a held core's sibling, and the drivers'
declarations say so.

**A shared-kernel tier does not survive a kernel privilege-escalation vulnerability.** True of every
container platform and true here, which is why the tier is the operator's choice: their own services
in `process` for density, arbitrary customer code in `microvm`, paid for in memory and boot time. No
configuration gives both, and Korpis does not imply one does. There is no unhardened container tier,
since a tier that looks like isolation and is not is worse than one honestly labelled `process`.

**The `microvm` row as built.** Each guest's Firecracker runs under the jailer in a chroot rebuilt
every run, as the workload's first id, in its own PID namespace, cgroup and network namespace, with
Firecracker's own seccomp filters never disabled, booting Korpis's build of a longterm kernel with
no PCI bus and a read-only root ([guests](guests.md)). **What a guest says to its host is read as
data of fixed layout**: the run's record is sixteen bytes, and the guest agent's frames are bounded
by kind and length before anything is read, so nothing a guest writes is parsed as a structure by
the root driver, except two bounded frames decoded by the same memory-safe decoder a host worker's
are: a file session's `FileResponse` and an install's `InstallResult`. The host listens on two vsock
ports of a guest with an identity directory and on none otherwise, relaying them unread to that
workload's own sockets, so a guest reaches nothing its workload could not as a container. The agent
answers for a workload that is root of its guest, so its answer is evidence of liveness and nothing
else. A service guest parses a tenant-formatted filesystem behind the hypervisor, holding only that
tenant's volumes. The isolation conformance cases are held for the maintainer, so this row is argued
from the implementation, not yet tested against it.

**A device given to a workload is its host driver given to the tenant.** A device in a `container`
is a node of the host's own kernel driver, so a flaw in that driver, and GPU drivers are large and
parse what a workload sends, is a kernel flaw the tenant can reach. CDI files and their hooks run as
root and are as trusted as the vendor's driver, which is why only root-owned files are read and the
driver reads a device's edits itself rather than from the agent
([runtimes](runtimes.md#what-a-driver-applies)). Timeslice holders share memory and can starve or
observe each other as far as the device allows; a `microvm` gets no device.

## Privilege separation on a node

```
supervisor          unprivileged; talks to the control plane, holds no root, touches no tenant path
privileged helper   root, small, no tenant input; an enumerated set of operations
filesystem workers  one per session, in a namespace holding only its volumes, Landlocked to them
backup jobs         one per backup or restore, one workload's key, only its work's capabilities
install jobs        one per install, as the workload's own ids, only that workload's volumes
runtime drivers     separate processes, each with only its own privileges
image builds        one per image, applying a publisher's layers as the workload's ids
ingress proxy       one per node, unprivileged and confined, the only parser of internet requests
```

A single process doing all of it holds the union of their privileges, so any bug anywhere is a bug
in a root process with every tenant's data. Split, a worker's bug is confined to one tenant's
subtree by the kernel. **What the split does not do:**

- **The `oci` driver is root-equivalent**: anything that can talk to containerd can ask for a
  privileged container with the host's root mounted. A compromised supervisor obtains only what the
  drivers accept, and they accept only specs inside their tier's invariants, so a driver's
  validation is part of the boundary and reviewed as such. containerd and runc are in the
  `container` tier's trusted base.
- **The helper is root.** Small and taking no tenant input makes bugs unlikely, not impossible.
- **A backup job reads tenant data with privilege**: `CAP_DAC_READ_SEARCH` to back up, and
  `CAP_CHOWN`, `CAP_FOWNER`, `CAP_DAC_OVERRIDE`, `CAP_FSETID`, `CAP_MKNOD`, `CAP_SETFCAP` to
  restore, uid 0 in the host's namespaces, confined by Landlock to one directory and the store's
  port and by a walk that never follows a link. A kernel without Landlock (before 5.13, or 6.7 for
  the port rule) leaves it confined by its own code, which its result says. Name resolution travels
  over UDP, which Landlock does not govern.
- **The ingress proxy holds the private keys of the endpoints it serves**, and on an edge member
  every name its group serves; a bug in it reaches those keys and the traffic through it, not the
  agent's session, the helper or workload data. HTTP/3 widens what can reach a bug in it, not what
  one reaches.
- **A client certificate the ingress accepts is not checked for revocation**, and the `Client-Cert`
  header is only as good as the path: a workload reached by a port it also exposes reads whatever a
  client sends.
- **A destination's receiving rsync during migration** runs as a user of its own, with only the
  capabilities to set ownership, in a mount namespace holding only its copy
  ([migration](migration.md)).

## Secrets

- **Nodes hold no long-lived registry credentials**: short-lived, digest-scoped pull tokens instead.
- **Nodes hold no standing object-storage credentials or repository keys**: one workload's data key
  and a prefix-scoped credential per job, for the job's length ([backup keys](backup-format.md)).
- **Tenant secrets never enter a template context**: a flat, enumerated map in a logic-less
  language, and secret fields delivered only through channels the tenant cannot read back; no
  ambient scope to walk.
- **Secrets are redacted before persistence**, on the node, labelled best effort
  ([streams](streams.md)).
- **Capability tokens are short-lived**, travel in a URL's fragment when shared, and never reach an
  extension's frame.
- **Nodes hold the TLS keys of the ingress endpoints placed on them and no others**, the control
  plane keeping every key sealed; and a block volume's credential only while its workload is placed
  there.
- **A node's own key** is per node, and its compromise is bounded below.

## What the operator can see

Tenants are not protected from the operator, who controls the hardware, kernel, storage and network;
any claim otherwise would be false. What Korpis provides is **visibility**, which is achievable and
changes behaviour:

- Operator access to a tenant's console, files or backups is an effect naming the operator, action
  and object, **visible to the tenant** in its own audit, and **no access is exempt**: an operator
  debugging a node-wide failure produces many effects, which is correct; volume is a display
  problem, solved by grouping effects under their operation. An operator wanting unobserved access
  has root and needs no supported path. A node its operator changed can serve a read and journal
  nothing: the effect makes the supported path visible, not the unsupported one.
- Break-glass access is a grant, issued, time-bounded and logged, not a bypass; a host reads a
  tenant's private data only through a grant naming the tenant, which the tenant sees issued and
  used.
- **Relays see the sessions they carry**: TLS terminates there, so console output, keystrokes and
  files in transit are plaintext on operator infrastructure. A relay's route key, stolen with a
  client's routes, says which node its workloads run on. **A relay serving SFTP can open a tenant's
  files as the tenant**: it checks the client's SSH key, which no library lets it prove, and the
  control plane mints a file token on its word, so a compromised one reads and writes files of
  anyone with a registered key, each token a `workload.file.sftp_authorized` effect on the tenant's
  trail; it is off until an operator enables it.
- **Tenant key custody limits backups, not the operator**: every opening is in the tenant's log, and
  revoking one key ends them all ([backup keys](backup-format.md#custody)).
- **An operator retention floor** for offloaded console segments (`--stream-retention-floor-days`)
  is disclosed to every reader beside the retention it results in, so an operator cannot silently
  retain more than declared; it applies off the node, never enlarging the tenant's quota.

## Compromise, and what it costs

**A compromised node.** Its blast radius is its tenants, including the backups of workloads placed
on it, which it could read live, the replicas it keeps of volumes running elsewhere, the
block-volume credentials of its workloads, and the credentials of its workloads, each lasting at
most fifteen minutes and ending when the workload is placed elsewhere. It reaches no other
workload's backups, no other node's assignments, and no registry beyond one artifact for minutes.
Recovery is one call, removing the node as compromised: its session ends and its key opens nothing,
its leases cannot be renewed and are over within one term, the next issue elsewhere raising the
epoch, so a compromised agent still running can reacquire nothing nor write an accepted observation.
Each workload is placed again from its target, with its data where its volumes are shared, block or
replicated, a block provider refusing the removed node at the new attach; local volumes are lost
with the node. It is not trusted to clean itself up: fenced and rebuilt.

It held the private keys of its endpoints, which outlive the fence. So the control plane, from its
record of every certificate key it gave a node (certificate, revision, leaf key digest, chain),
marks each such certificate exposed with a critical `certificate.exposed` event, gives an `acme` one
a new key at once, and revokes every chain the node was given through the directory with
`keyCompromise`, asked until answered, each answer told as a `certificate.revoked` event or a
`certificate.revocation_refused` warning. A `provided` certificate stays exposed until its
organization replaces it with a chain for another key. Revocation reaches only clients that check,
many do not, so the protection that holds regardless is a certificate's short life.

**A compromised edge member** sees every connection to the addresses it holds, terminates every name
its group serves and holds those keys. Into a workload's node it reaches only the ports its group
forwards there and, through the mesh, the `ingress` ports its group serves, which a client or its
proxy would carry anyway. A probe does not catch a member that forwards the probe and drops the
rest. Recovery is a node's.

**A compromised control plane** is total: it holds the authority to issue grants. What is bounded is
reconstruction: restore fences with a monotonic `max_issued_epoch` watermark and rotates the signing
key, so a restored control plane can neither issue an epoch a live agent accepts as current nor
honour a token it has no record of ([control plane restore](control-plane-restore.md)).

**A compromised tenant account** is bounded by its subject's grants, enumerable, individually
revocable, and every use in the effect log. There is no admin flag to inherit.

**Attestation** that a node runs the kernel and agent it claims would raise compromise detection, at
the price of TPM, measured boot and key management disproportionate to a single-operator install; it
is not designed.

## Egress is enforced by the party contained

Egress rules are applied at the source node by its agent, which a compromised agent can ignore. An
attacker who reached the helper or a root-equivalent driver has root on the machine and can rewrite
nftables, read every tenant on it and forge observations; egress is the least of what was lost, and
the answer to node compromise is revoke, fence, rebuild, not a cleverer firewall. But:

**East-west is enforced at both ends.** The source drops early; the destination enforces because it
does not trust the source. Across nodes WireGuard accepts a packet only from a source in the sending
peer's own pool, which is what the control plane gave that node, so a compromised node cannot speak
as another, and the destination admits from the mesh only its workloads' dependents to their
dependencies' ports ([mesh](mesh.md#both-ends-hold-the-policy)); it reaches what any dependent on it
is admitted to, which it could reach through that dependent anyway. Every node's other doors hold
too: from outside the node only what it translated and ports exposed at the workload's own address
are let in.

**North-south is uncontained on a compromised node**, stated rather than implied: only the source or
a chokepoint the operator owns can enforce it. So Korpis publishes each node's expected egress
profile in machine-readable form, for an operator enforcing at a layer the node does not control (a
VLAN, a security group, an upstream ACL).

## Resource exhaustion, indexed

An unbounded resource is a denial of service with a friendlier name.

| Resource | Bound | Where |
|---|---|---|
| CPU, memory | cgroup weight, quota, `memory.max`; reservations never oversubscribed | [resources](resources.md) |
| processes and threads | `pids.max`, always: the fork bomb | [resources](resources.md#io-and-processes) |
| disk bytes, inodes | filesystem quota in the write path | [storage](storage.md) |
| disk and backup I/O | `io.max` and shares where enforceable; backup jobs throttled | [storage](storage.md#io-limits) |
| bandwidth | shaping per workload and endpoint | [bandwidth](bandwidth.md) |
| conntrack entries | per-workload limit: a shared table is a shared failure | [network policy](network-policy.md) |
| ports and addresses | explicit allocation, a placement and quota dimension | [endpoints](endpoints.md) |
| resolver queries | 50 a second per workload, burst 200 | [discovery](discovery.md) |
| log volume and storage | a rate limit with gap records; counted against disk quota | [streams](streams.md) |
| metric cardinality | tenant-controlled values are never labels | [observability](observability.md) |
| API calls, control-plane work | per subject and organization, never multiplied by attenuation; expensive budgets | [rate limits](rate-limits.md) |
| unauthenticated attempts, recorded denials | per source; guessable secrets per account; coalesced | [rate limits](rate-limits.md) |
| node journal | a declared size, coalescing, then a gap | [node agent](node-agent.md) |
| provider capacity | per-provider concurrency and a breaker on latency | [providers](providers.md) |
| outbound abuse | internal destinations denied, internet rate-limited, SMTP denied | [network policy](network-policy.md) |

## Supply chain and disclosure

| Arrives from outside | Verification |
|---|---|
| recipes | cosign signature under the organization's trust policy, digest-pinned; a `fetch` without a hash does not parse |
| extensions | the same, with declared grants approved at install |
| images | digest-pinned; tags resolved once and recorded |
| Korpis's binaries | reproducible builds, signed releases, published SBOM |

Imported eggs are graded rather than refused: `unverified` ones install in a microVM with no network
beyond confirmed hosts, and an import path pretending imported artifacts are trustworthy would undo
this page. Reputation is the operator's: `accepted_signers` per organization and an extension
publisher allow-list; nothing central.

A security policy, a contact and a response window exist before the first release, since a project
without a disclosure path receives its vulnerabilities as public posts. Advisories carry the detail
Korpis's own research relied on: what was reachable, what it affected, which versions, and what the
fix changed.
