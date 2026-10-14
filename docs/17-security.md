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
runtime drivers     separate processes, each holding only its own driver's privileges.
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
  reach nothing else; a target
  that cannot issue such a credential is written through a relay instead (§5.5 of
  `06-storage.md`). The first version said nodes held no object-storage credentials while the
  backup flow streamed encrypted chunks from the node straight to the repository, which required
  both a key and a credential on the node (finding S37). Now the node holds both, for one workload
  at a time, for the length of one job.
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
- **The agent's own key** is per-node, rotatable, and its compromise is bounded by §9.

---

## 8. What the operator can see, stated plainly

Tenants are not protected from the operator. The operator controls the hardware, the kernel, the
storage, and the network; any claim otherwise would be false, and P4 forbids it.

What Korpis provides instead is **visibility**, which is achievable and is what actually changes
behaviour:

- Operator access to a tenant's console, files, or backups produces an `Effect` naming the
  operator, the action, and the object (§4 of `15-observability.md`).
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
- **A host reads a tenant's private data only through a grant that names the tenant**, which the
  tenant sees issued and sees used (§3.3 of `08-identity.md`).

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
for a workload placed elsewhere. Recovery: revoke the node
key, and **increment the lease epoch**. §4.5 of `02-architecture.md` and §8 of `03-state.md` make
epochs monotonic and fencing mandatory, so a compromised agent that keeps running cannot reacquire
ownership of anything, and cannot write an observation the control plane will accept. Workloads are
rescheduled elsewhere from their intents, which are unaffected. The node is not trusted to clean
itself up; it is fenced and rebuilt.

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
| unauthenticated attempts | per source and per targeted account or link, slowing rather than locking | §10 of `10-api.md` |
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
   and they are what defends against them.** Not defended against by default; §5 says so plainly.
   Whether Korpis offers core pinning and sibling-thread exclusion as a declared workload property
   (with the density cost stated) was unresolved, and the default advice was to pick `microvm`,
   which on its own does not stop the sibling leaks (finding S46).

`cpu.isolation: dedicated_cores` (§5.2 of `05-scheduling.md`) pins a workload to whole physical
cores and keeps every other tenant off their SMT siblings. It is honest on both counts: it is the
only configuration that defends against the cross-core leak classes, in any tier, and it costs the
whole core, which the scheduler accounts and the meter bills. Combined with `microvm` it is the
strongest isolation Korpis offers; neither alone is.
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
