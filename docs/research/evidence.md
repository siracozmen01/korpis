# Evidence

**Status:** research input for the Korpis design **Date:** 2026-08-07, **revised** 2026-10-12

> Revised after the second external review (§17 of `../23-walkthroughs.md`), which re-checked every
> cited advisory and issue against GitHub's API on 2026-10-12. Nine citations were wrong: an issue
> described as unanswered had been answered the same day, five issues cited as current defects had
> been fixed or were not defects, one advisory was counted twice, one fix version was wrong, and the
> advisory count itself was wrong. They are corrected in place
> below, with what changed. The advisory table now covers every advisory published to that date.

Korpis departs from how this class of software is normally built, and a design document that says
"we did it differently" without saying *why* is just taste. This is the file that keeps the rest
honest: Rules K-1 to K-12 in §6 of `00-overview.md` trace to something specific and citable here,
and §4 says what each of the later rules rests on instead.

**On tone, because this matters.** The systems examined below are successful. Pterodactyl in
particular solved a real problem for a large number of people and is why this market exists in a
usable form at all; a great deal of what Korpis does is Pterodactyl's idea. Every advisory listed
here was responsibly disclosed and fixed by maintainers doing unpaid work. The issues cited are a
different matter: some are open, some were declined, and some were fixed, and each one says which.

The point of collecting them is not that these projects have bugs, everything has bugs. It is that
**certain failures recur**, in the same shape, across years and across independent codebases, and
recurrence is the signature of an architecture that invites the failure rather than of a developer
who was careless. Those are the ones worth designing around, and the only way to tell them apart
from ordinary bugs is to count.

If you maintain one of these projects and think something here is wrong or unfair, please open an
issue. Being accurate matters more to this document than being right.

Every claim is tagged with a confidence level:

- **[V]** Verified against a primary source (GitHub issue, official docs, security advisory).
- **[R]** Reported by a secondary source that looks credible but was not independently confirmed.
- **[W]** Weak: appeared only in SEO/marketing content. Recorded for completeness, not to be relied
  on.

A note on method: a large share of search results for this topic are hosting-company blog spam.
Several contained outright errors (one claimed Wings is a Node.js process; it is Go). Claims from
those sources were either dropped or demoted to **[W]**.

---

## 1. Pterodactyl

### 1.1 The security record points at a few architectural flaws, not twenty bugs

Every published Wings advisory to 2026-10-12, from GitHub's advisory database: **[V]**

| ID | Published | Severity | Title | Class |
|---|---|---|---|---|
| GHSA-6rg3-8h8x-5xfv | 2021-01 | Moderate | Unchecked hostname resolution allows access to local network resources | SSRF |
| GHSA-jj6m-r8jc-2gp7 | 2021-06 | Moderate | Resource amplification in containers created by Wings (CVE-2021-32699), fixed 1.4.4 | **limit not held** |
| GHSA-66p8-j459-rq63 | 2023-02 | Critical | Improper link resolution allowing deletion of host files (CVE-2023-25168), fixed 1.7.4 / 1.11.4 | **path** |
| GHSA-p8r3-83r8-jwj5 | 2023-02 | High | Improper link resolution allowing creation of host files (CVE-2023-25152), fixed 1.7.3 / 1.11.3 | **path** |
| GHSA-p744-4q6p-hvc2 | 2023-05 | Critical | Escape to host from installation container (CVE-2023-32080), fixed 1.7.5 / 1.11.6 | install code |
| GHSA-494h-9924-xww9 | 2024-03 | Critical | Symlink race in server filesystem (CVE-2024-27102) | **path** |
| GHSA-gqmf-jqgv-v8fw | 2024-05 | High | Arbitrary file write/read (CVE-2024-34066) | **path** |
| GHSA-qq22-jj8x-4wwv | 2024-05 | Moderate | SSRF during remote file pull (CVE-2024-34068) | SSRF |
| GHSA-2497-gp99-2m74 | 2026-01 | High | Endless reprocessing of activity log data (CVE-2026-21696) | other |
| GHSA-rhq6-9rgh-v45c | 2026-05 | Moderate | Chmod can change permissions of files outside the server container | **path** |
| GHSA-pfvc-3p5h-x7h6 | 2026-06 | Critical | Node secrets exposed through egg configuration-file templating (CVE-2026-52855, CVSS 9.9), fixed 1.12.3 | templating |
| GHSA-ghrq-5wpp-hxx5 | 2026-06 | High | Crafted SFTP handshake packet causes denial of service (CVE-2026-52856) | other |
| GHSA-q6hh-gp44-4hcm | 2026-06 | Moderate | Malformed parsed config files cause Wings OOM (CVE-2026-52857) | other |
| GHSA-77pg-7gfq-9hjr | 2026-08 | Critical | Unsanitized backup identifiers permit host path traversal (CVE-2026-104261) | **path** |
| GHSA-hfvm-879c-xgmm | 2026-08 | High | SFTP streaming writes bypass per-server disk quotas (CVE-2026-104260) | **limit not held** |
| GHSA-5mgr-9frx-2rh7 | 2026-08 | High | Integer overflow in upload and archive quota calculations bypasses disk quota (CVE-2026-104263) | **limit not held** |
| GHSA-8j54-xcwx-597p | 2026-08 | High | SFTP write path enforces disk quota only as a stale pre-flight check (CVE-2026-61617) | **limit not held** |
| GHSA-7w9q-4q32-wfj8 | 2026-08 | High | Backup restore URLs permit SSRF (CVE-2026-104262) | SSRF |
| GHSA-4g8v-75pm-x7f6 | 2026-08 | High | Prefix-based registry matching leaks credentials to attacker registries (CVE-2026-104265) | credentials |
| GHSA-p682-235x-522p | 2026-10 | Moderate | Nil-pointer dereference in SFTP Setstat crashes the daemon | other |
| GHSA-grp4-3hv5-m6vf | 2026-10 | Moderate | Token revocation not enforced on signed-URL file and backup handlers | revocation |

**The lesson is not "Wings has bugs."** Twenty-one advisories fall into a handful of recurring
classes, and the two largest are architectural:

- **Path (six).** A privileged process performs filesystem operations on paths supplied by an
  untrusted tenant and defends itself by *validating path strings*. Path-string validation loses
  to symlinks, races, and `..` every time; it is a losing strategy, not a series of oversights.
- **A limit displayed and not held (four).** Resource limits in containers, and three distinct ways
  around the disk quota in August 2026 alone: SFTP streaming, integer overflow in the arithmetic,
  and a quota checked before a write rather than in it.

Then SSRF (three), and one each for install code, templating, credentials scoping, and revocation.
The install-code and templating advisories are the classes that §4 and §6 of `09-recipes.md`
remove.

> Corrected 2026-10-12. This section said "six of the twelve" advisories were the path class. The
> table then listed ten, with CVE-2023-32080 added as an eleventh issue although it is the CVE of
> GHSA-p744 already in the table, and omitted GHSA-6rg3. On the document's own date (2026-08-07)
> thirteen advisories had been published, five of them in the path class. The fix version given
> for CVE-2023-25152 (1.7.4) was the fix for CVE-2023-25168. The table above is the full record.

> **Rule K-1.** The component that touches tenant files must never hold host privilege. Filesystem
> access is confined by the kernel, a per-tenant mount namespace plus `openat2(RESOLVE_BENEATH)` or
> an equivalent, never by string inspection. Path validation is defence in depth, not the defence.

The critical advisory GHSA-pfvc-3p5h-x7h6 deserves separate attention: egg configuration-file
**templating** could be made to render node secrets, the daemon token and Docker registry
credentials, into a file the tenant can read. It is CVE-2026-52855, CVSS 9.9, fixed in Wings 1.12.3
**[V]** (the advisory itself; this was tagged [R] when only a secondary source had been read). The
root cause is that the template evaluation context and the daemon's secret context were the same
context.

> **Rule K-2.** Tenant-authored templates evaluate in a sandbox with an explicit, allow-listed
> variable set. There is no ambient scope to leak from.

### 1.2 Resource limits are advertised but not enforced

- **Disk quota is not enforced in the write path.** Issue #4554: an 858 GB file existed on a server
  configured with a 1 GB disk limit. A maintainer explained in 2022 that containers do not provide
  disk limits and Wings checks usage on an interval; the issue was then **closed as not planned** in
  May 2024, without a closing comment, in a batch of 86 issues closed that day. **[V]** The August
  2026 advisories above are the same property failing three more ways. In 2018, #1050 asked for
  stricter disk restrictions; maintainers answered that usage is checked at start and every 30
  seconds thereafter, and that anything stricter belongs to external monitoring. **[V]**
- **No per-tenant disk I/O limit.** Wings throttles its **backup** throughput, added in December
  2020 to fix #2798 (backups causing iowait spikes across the node), but a tenant's own I/O has no
  limit. **[V]**
- **No bandwidth limit.** #1871 (limit bandwidth per server) was declined until Docker gains the
  capability, and #3352 was answered with "look into Linux utilities". **[V]**

CPU and memory are handled through cgroups. Disk capacity and network are not limited per tenant,
and disk I/O is limited only for backups. On a multi-tenant node that means one tenant can degrade
or halt every other tenant.

> Corrected 2026-10-12. This section cited #2638 (servers not stopped at the disk limit) and #2798
> (backup iowait) as current defects; both were fixed in 2020. It cited #3780 (no network usage
> chart), which was implemented in 2022, and #5186 as the panel misreporting disk usage, which on
> reading turned out to be space used outside the server's volume, correctly reported. Those four
> citations are removed or restated as fixed.

> **Rule K-3.** Every limit Korpis displays is enforced by the kernel, or Korpis does not display
> it. Capacity, IOPS, bandwidth, and PIDs are first-class limits alongside CPU and memory, and each
> is metered.

### 1.3 Server transfer has been broken for years

Transfer between nodes is the mechanism for hardware maintenance, node decommissioning, and
capacity rebalancing. Its issue history: **[V]**

- #18, #280: original feature requests
- #3015: transfers fail with a "walking error"
- #3332: `processFailedTransfer()` fails on a NULL argument
- #4244, #4614, #4644: assorted transfer failures
- #4505: **a failed transfer deadlocks the server**: unusable from client and admin panel alike,
  state cannot be overridden, every state change returns HTTP 409. Recovery requires truncating the
  `server_transfers` table by hand.
- #5429: symlinks are silently dropped, so the server will not boot at the destination

An independent developer cites a transfer bug that stayed broken for 40 days with a fix available
and no guidance issued to affected installations. **[R]**

The root cause is structural: transfer is written as a single-shot imperative operation. There is
no resumable state machine, no verification step, and no defined rollback, so any interruption
leaves the system in a state the model cannot express.

> **Rule K-4.** Migration is a first-class, resumable, restartable job with explicit phases
> (snapshot, ship, verify, cut over, release), each independently retryable. No operation may leave
> a workload in a state the model cannot represent. There is always a path back.

### 1.4 Backups do not scale

- **Full backups only.** No incremental support; open feature requests for incremental (discussion
  #5493) and for Borg (discussion #4715). A 40 GB server means writing 40 GB every time. **[V]**
- **Backups stage to local disk first** (#3846, open), so the node needs free space equal to the
  backup. **[V]**
- **Backup settings cannot vary per location** (#4411, closed in 2026 when feature requests moved to
  Discussions, not implemented). **[V]**
- S3 uploads above 5 GB failed until Wings 1.1.0 (November 2020) switched to multipart upload
  (#2599, fixed). And backup I/O degraded the whole node until a throughput limit was added in
  December 2020 (#2798, fixed). Both are history rather than current behaviour, and are kept
  because they are the same lesson: backup bolted onto a plain directory has to rediscover every
  scaling limit one at a time. **[V]**

> Corrected 2026-10-12. #2599 and #2798 were cited as current defects; both were fixed in 2020.

> **Rule K-5.** Backup is a consequence of the storage design, not a feature bolted on top. Choose
> a filesystem that snapshots (ZFS/btrfs subvolume per workload), take the snapshot instantly, then
> ship it content-addressed, deduplicated, and encrypted. The workload never stops, the node never
> stages a full copy, and incremental is the default rather than a feature request.

### 1.5 There is no extension API, so the ecosystem patches core files with `sed`

Blueprint is the de facto extension framework, and its own documentation describes the mechanism:
extensions ship shell scripts that run at install/update/remove/export time and modify
Pterodactyl's core files by **targeted string substitution**, replace `foo` with `bar` on install,
replace `bar` with `foo` on removal. Extensions are told not to overwrite files, not to run remote
scripts, not to obfuscate code, and to stay inside `$PTERODACTYL_DIRECTORY`. **[V]**

Every one of those constraints is a social rule, not a technical boundary. The framework cannot
enforce any of them: the scripts run with the installer's privileges.

Observed consequences: extensions break on panel updates, webpack module resolution errors after
upgrades, theme conflicts between extensions, extension settings lost on reinstall unless the
developer persisted them to the database, and installs that wedge in a "you need to finish
installing Blueprint" state. **[V]**

`update.sh` runs from the *pre-update* copy of the extension, not the new one, a detail that
guarantees subtle breakage for anyone who does not read that specific documentation page. **[V]**

An independent developer building a from-scratch alternative observed that virtually every serious
deployment depends on Blueprint, and concluded core extensibility should have been present from day
one. **[R]**

> **Rule K-6.** Extensions are separate processes speaking a versioned contract, holding
> narrowly-scoped tokens, and contributing UI through declared slots. An extension must never be
> able to modify Korpis' own files. Core features use the same extension mechanism third parties
> use, if the mechanism is not good enough for core, it is not good enough to ship.

### 1.6 The panel and the daemon were never decoupled

Issue #141, opened **October 2016**, asked for third-party daemon support. It was answered the same
day: this was "already technically possible so long as the daemon you are using follows the same
API style as the core daemon", and on closing it three days later the maintainer added that
supporting external daemons is difficult "unless said daemon is specifically designed to work with
Pterodactyl". **[V]** That is the finding: the contract was the reference implementation's
behaviour. Panel and Wings are coupled by version rather than by a published protocol, so they must
be upgraded in lockstep, and an alternative data plane can only exist by imitating Wings.

> Corrected 2026-10-12. Earlier versions of this paragraph, of §1 of `02-architecture.md`, and of
> §1 and §7 of `19-governance.md` said #141 had been open and unanswered since 2016. It was answered
> and closed within three days.

> **Rule K-7.** The control plane and the data plane are separated by a published, versioned,
> independently implementable protocol with explicit compatibility guarantees. A third party must
> be able to write a conforming agent without reading Korpis' source.

### 1.7 Governance was the actual failure

Pelican was forked in early 2024 (its panel repository dates from March 2024, **[V]**), primarily
because Wings had gone a long stretch without updates and the fork wanted to ship releases and
security fixes faster. **[R]** Pelican relicensed the panel from MIT to AGPLv3, which pushed
commercial hosts relying on proprietary differentiation to look elsewhere. **[R]** Pterodactyl has
continued under MIT, and its leadership has publicly framed the current phase as cleanup and
maintenance (patching holes, untangling dependencies, improving testing) before new features.
**[R]**

A candid outside assessment described Pterodactyl's release process as "almost like it's a hobby
project," with weak testing infrastructure and no coordinated triage or review, notable for
software that became an industry default. **[R]**

> Corrected 2026-10-12. The first sentence said Pelican was forked "in mid-2024"; its repository
> was created on 2024-03-19.

> **Rule K-8.** License, versioning policy, compatibility guarantees, and maintainership, including
> what happens when a maintainer stops, are day-one decisions, written down before the first
> release. An extension ecosystem can only exist on top of a stable, promised contract.

> Corrected 2026-10-12. K-8 said "a multi-maintainer structure". The evidence above supports
> writing maintainership and succession down before the first release; it does not make a second
> maintainer something a project can decide into existence. Korpis has one maintainer, and §6 of
> `19-governance.md` says so and says what follows from it.

---

## 2. What the forks proved

### Pelican: the ceiling of forking

Shipped, by their own comparison: a first-party plugin system, native webhooks, roles and
permissions for admins, tags replacing the nest/location system, Filament rewrites of both client
and admin UI, Vite instead of Webpack, integrated API documentation with broader coverage, OAuth,
PostgreSQL/SQLite/MariaDB drivers, per-user timezones, IPv6 allocations, and servers without
allocations. **[V]**

Still listed as *planned*, not done: **allocation system rework**. **[V]**

That single line is the most informative sentence in this document. Pelican fixed nearly every
surface-level complaint about Pterodactyl and still has not touched the networking model, because
`(ip, port)` allocations are load-bearing for the entire schema. Some things cannot be fixed by
forking.

### Pyrodactyl: you cannot fix architecture from the frontend

Claims a bundle over 170× smaller than other forks including Pelican, load times ~16× faster,
rendering up to 70% faster, cold builds under 7 seconds. **[R]** Real, measurable, and entirely
frontend: Vite, code splitting, chunking, caching. The Laravel backend and Wings daemon are
unchanged.

Pyro subsequently handed Pyrodactyl to the community and announced a new platform built from the
ground up. **[R]** The most capable performance-focused fork concluded that the remaining problems
were not reachable from a fork.

### TheGamePanel: an independent from-scratch attempt

Cited reasons: poorly written code duplicating framework functionality, many logical
inconsistencies, repeated security vulnerabilities and critical bugs, the 40-day transfer bug,
missing core extensibility, and hobby-grade release process. Approach: build from scratch, minimal
dependencies, true modularity where core features use the same extension system plugins use,
self-contained binary via FrankenPHP, while keeping backward compatibility with existing
Pterodactyl infrastructure. **[R]**

**Three independent teams (Pelican, Pyro, TheGamePanel) reached the same conclusion from three
different directions: the architecture cannot be fixed in place.** That is the strongest available
evidence that Korpis should start from a new model rather than a fork.

---

## 3. Competitors

### AMP (CubeCoders): commercial, and holds the market Pterodactyl cannot enter

Runs on **Windows and Linux**, Docker optional, supports 400+ games, and is widely reported as
easier to set up. **[R]**

> Pricing removed 2026-10-12. This paragraph quoted AMP's licence prices, which `CLAUDE.md` keeps
> out of this repository: competitor pricing is business-model analysis, not technical evidence.

Windows support is the whole story. Pterodactyl's hard dependency on Docker excludes Windows-only
game binaries, several anti-cheat systems, and workloads needing GPU or custom kernel modules.
**[R]** AMP and TCAdmin hold that segment essentially unopposed by open-source alternatives.

> **Rule K-9.** The runtime is an interface, not a dependency. The interface must be able to
> express a Windows execution model on day one even if no Windows driver ships for a year,
> retrofitting it later means rewriting every consumer.

### PufferPanel: the counter-example

Go, lightweight, and Pterodactyl's own ancestor. Runs game servers as **processes directly on the
host OS** rather than in containers: lower overhead, broader platform compatibility, and
substantially weaker isolation, since an exploited game server can affect other services on the
same machine. **[R]**

Pterodactyl hardcoded containers; PufferPanel hardcoded no containers. Both froze an isolation
decision that belongs to the operator.

> **Rule K-10.** Isolation strength is a per-workload property chosen by the operator, shared
> kernel container, hardened container, microVM, full VM, or bare process, not a property of the
> platform.

### Coolify: the general-purpose comparison

The closest thing to a general-purpose Korpis competitor, and instructive mostly by its limits: no
automatic failover between nodes, nodes must be uniformly AMD64 or uniformly ARM (no mixing), no
seamless migration between self-hosted and managed, a control plane that idles heavy because it
runs its own supporting containers, and no managed databases. **[R]** Eleven critical
vulnerabilities are reported as disclosed in January 2026 including command injection, root key
leakage, and a **default root-level process model**. **[W]**, single-source, treat as directional
only.

Architecturally Coolify assumes one workload shape: an HTTP service behind a proxy. That assumption
is as limiting as Pterodactyl's "long-running process with a console," just in the opposite
direction.

> **Rule K-11.** Workload shape is a field on the workload, not an assumption baked into the
> platform. `console`, `service`, `job`, `cron`, `stateful`, and `machine` are all first-class, and
> they share one scheduler, one storage layer, one network layer, and one identity model.

### Agones: the only mature scheduler model in this space

Google/Ubisoft, announced 2018, a Kubernetes extension for dedicated game servers. Worth stealing
from: **[V]**

- **Fleet**: a set of warm, Ready workloads available for allocation, with a declared replica
  count.
- **Scheduling policy as an explicit choice**: `Packed` bin-packs onto as few nodes as possible
  (right for elastic cloud capacity, enables scale-down); `Distributed` spreads across the cluster
  (right for fixed hardware). Pterodactyl has neither: node selection is manual.
- **Batched allocation**: allocation requests are accumulated for ~500 ms and committed together
  rather than one round trip to the API server per allocation.

Agones is Kubernetes-native, which makes it far too heavy for Korpis' target operator. The *model*
is the part worth taking.

### Nomad: the reference design for a pluggable runtime

Nomad's task driver plugin interface is the closest existing thing to Rule K-9 and should be
studied directly rather than reinvented: **[V]**

- Drivers declare **capabilities** (`SendSignals`, `Exec`, …) so the scheduler knows what a driver
  can do instead of assuming.
- `ConfigSchema` lets a driver publish its own configuration schema; `Init` runs setup after
  `SetConfig`; `ExecTask` runs commands inside the task's execution context.
- Shipped drivers span the full isolation range: `docker` (namespaces + cgroups), `exec` (cgroups +
  chroot), `exec2` (**Landlock LSM + cgroups v2 + `unshare`**, no container runtime at all), and
  `qemu` (full virtualization, resources bounded by the hypervisor rather than the host kernel).

`exec2` is especially relevant: it demonstrates strong isolation on modern kernels without a
container runtime dependency, which is exactly what a `native` driver for Korpis needs.

### Billing: permanently external, permanently shallow

WHMCS, Blesta, Paymenter, WemX, BillingServ, PteroBill. Paymenter has native Pterodactyl
provisioning but limited integration breadth and shallower automation than WHMCS; its cPanel
extension, for comparison, covers the provisioning lifecycle but has no package sync, no SSO, and
no in-panel management. **[R]**

The pattern across the whole category: billing systems can create and delete, and can do almost
nothing else, because the panel exposes provisioning but not identity, metering, or lifecycle.

> **Rule K-12.** Korpis does not implement billing. Korpis implements what billing needs and never
> gets: organizations and teams as first-class objects, plans and quotas as enforced policy, usage
> metering as an event stream (CPU-seconds, GB-hours, egress GB, IOPS), a typed event stream for
> every state change, and single sign-on. A billing integration should be a few hundred lines and
> require no fork.

---

## 4. Consolidated rules

Rules K-1 to K-12 are derived from the evidence above. Rules K-13 to K-18 were added while the
design was written, and they are not derived from an advisory or an issue: each is a policy or a
design decision, and its basis is stated so that it can be argued with. Earlier versions of §6 of
`00-overview.md` said all of them were traceable to evidence; that was corrected on 2026-10-12.

| # | Rule | Kind | Comes from |
|---|---|---|---|
| K-1 | Tenant filesystem access is confined by the kernel, never by path-string validation | evidence | The largest advisory class: 6 of 21 Wings advisories (§1.1) |
| K-2 | Tenant templates evaluate in a sandbox with an allow-listed variable set | evidence | GHSA-pfvc-3p5h-x7h6 (§1.1) |
| K-3 | Every displayed limit is kernel-enforced and metered, or it is not displayed | evidence | #4554, #1050, #1871, #3352; four limit advisories (§1.1, §1.2) |
| K-4 | Migration is a resumable, verifiable, reversible job with explicit phases | evidence | #4505, #5429, #3332, #3015 (§1.3) |
| K-5 | Backup is a consequence of the storage design | evidence | #5493, #4715, #3846, #4411; #2599 and #2798 as history (§1.4) |
| K-6 | Extensions are out-of-process, versioned, sandboxed; core uses the same mechanism | evidence | Blueprint's `sed` model (§1.5) |
| K-7 | Control plane and data plane are separated by a published, versioned protocol | evidence | #141 and its answer (§1.6) |
| K-8 | License, versioning, compatibility, and maintainership are day-one decisions | evidence | Pelican fork, MIT→AGPL split, release process (§1.7) |
| K-9 | The runtime is an interface; it must express Windows on day one | evidence | The Windows segment held by AMP and TCAdmin (§3) |
| K-10 | Isolation strength is a per-workload operator choice | evidence | Pterodactyl vs PufferPanel (§3) |
| K-11 | Workload shape is a field, not an assumption | evidence | Coolify vs Pterodactyl (§3) |
| K-12 | Korpis implements what billing needs, not billing | evidence | WHMCS/Paymenter integration depth (§3) |
| K-13 | Never price per workload, node, instance, account, or domain | policy | Principle P10 and §2 of `00-overview.md`: Korpis is free, with no edition matrix |
| K-14 | Tenancy is recursive delegation; reselling is an emergent property | design | §3 Billing: resale exists today only as provisioning glue, because panels expose no identity |
| K-15 | Recipes are content-addressed and fetched once per node, not once per workload | design | Image and artifact caching as in containerd and Nomad; GHSA-p744 shows fetch-at-install is also a trust problem |
| K-16 | Health is application-level, not process-level | design | Agones' Ready state (§3); a running process is not a serving game server |
| K-17 | Configuration is schema-driven, with per-field permissions | design | Egg variables are untyped strings with no per-field authority (§1 of `09-recipes.md`) |
| K-18 | Ship one excellent runtime driver before five adequate ones | design | Build-order judgement for a single maintainer; argued under rule D in §2 of `20-roadmap.md` |
| ~~K-19~~ | Retired 2026-10-12: "licensing is decided before the first release" restated K-8 | | |

---

## 5. Sources

Primary:
- [Pterodactyl Wings security advisories](https://github.com/pterodactyl/wings/security/advisories)
- GHSA-p744-4q6p-hvc2 (CVE-2023-32080), escape to host from installation container:
  [advisory](https://github.com/pterodactyl/wings/security/advisories/GHSA-p744-4q6p-hvc2)
- GHSA-p8r3-83r8-jwj5 (CVE-2023-25152), improper link resolution:
  [advisory](https://github.com/pterodactyl/wings/security/advisories/GHSA-p8r3-83r8-jwj5)
- GHSA-pfvc-3p5h-x7h6 (CVE-2026-52855), node secrets through configuration-file templating:
  [advisory](https://github.com/pterodactyl/wings/security/advisories/GHSA-pfvc-3p5h-x7h6)
- Every other advisory in the §1.1 table is at the same address with its GHSA identifier.
- [#4554: disk space attack, 1 TB+ file, closed as not
  planned](https://github.com/pterodactyl/panel/issues/4554)
- [#1050: more restrictions on disk usage](https://github.com/pterodactyl/panel/issues/1050)
- [#2798: high iowait and memory usage during backups, fixed in
  2020](https://github.com/pterodactyl/panel/issues/2798)
- [#1871: limit bandwidth per server](https://github.com/pterodactyl/panel/issues/1871)
- [#3352: network limit per server](https://github.com/pterodactyl/panel/issues/3352)
- [#4505: failed transfer causes server deadlock](https://github.com/pterodactyl/panel/issues/4505)
- [#5429: transfer does not move symlinks](https://github.com/pterodactyl/panel/issues/5429)
- [#3332: processFailedTransfer NULL argument](https://github.com/pterodactyl/panel/issues/3332)
- [#3015: transfers failing, walking error](https://github.com/pterodactyl/panel/issues/3015)
- [#2599: S3 backups cannot exceed 5 GB, fixed in Wings
  1.1.0](https://github.com/pterodactyl/panel/issues/2599)
- [#4411: backup settings per location](https://github.com/pterodactyl/panel/issues/4411)
- [#3846: backups upload to S3 without local
  disk](https://github.com/pterodactyl/panel/issues/3846)
- [#5493: incremental backup support](https://github.com/orgs/pterodactyl/discussions/5493)
- [#4715: Borg backup feature request](https://github.com/orgs/pterodactyl/discussions/4715)
- [#141: third-party daemon support, answered and closed in October
  2016](https://github.com/pterodactyl/panel/issues/141)
- [Blueprint: extension scripts documentation](https://blueprint.zip/docs/concepts/scripts)
- [Pelican: comparison with Pterodactyl](https://pelican.dev/docs/comparison/)
- [Agones: overview](https://agones.dev/site/docs/overview/), [fleet
  spec](https://agones.dev/site/docs/reference/fleet/), [allocation
  spec](https://agones.dev/site/docs/reference/gameserverallocation/)
- **[V]** containerd's shim ends a `binary://` logging process by closing the task's pipes, then
  sending SIGTERM, then killing it if it has not exited within twelve seconds
  (`binaryIOProcTermTimeout`), checked 2026-10-14 against
  [cmd/containerd-shim-runc-v2/process/io.go](https://github.com/containerd/containerd/blob/main/cmd/containerd-shim-runc-v2/process/io.go).
  What the stream writer's last drain is sized against (§4 of `../14-streams.md`).
- **[V]** Rust's `regex` crate runs every search in time linear in the pattern and the input,
  with no backtracking, and bounds the memory a compiled pattern may take, checked 2026-10-14
  against the crate's documentation ([regex: untrusted
  input](https://docs.rs/regex/latest/regex/#untrusted-input)). What makes a reader's search
  pattern safe for the node to run (§8 of `../14-streams.md`).
- **[V]** On btrfs with qgroups on and consistent, destroying the stale qgroup a deleted
  subvolume leaves behind (`btrfs qgroup destroy`, after `btrfs subvolume sync`) marked the
  qgroup accounting inconsistent at the next commit, after which a 1 MiB qgroup limit let 2.5 MiB
  through and the accounting read zero until a rescan. Without the destroy, the same sequence
  stayed consistent and the limit refused the write with `EDQUOT`. Reproduced 2026-10-14 on a
  loop-mounted filesystem, Linux 6.8.0, btrfs-progs 6.6.3. Why the helper never destroys a qgroup
  and enforces only while the accounting is consistent (§3 of `../06-storage.md`).
- **[R]** Cilium enforces a policy that names a DNS name (`toFQDNs`) by proxying the workload's
  DNS queries, learning the addresses each answer gives, and allowing those addresses for the
  answer's lifetime ([Cilium: DNS-based
  policies](https://docs.cilium.io/en/stable/security/dns/)); recorded from its documentation and
  not reproduced here. One way a node could hold a rule naming a domain, the open question 8 of
  §11 of `../07-networking.md`.
- **[V]** Pterodactyl's panel is written in React and TypeScript: its `package.json` on the
  `develop` branch depends on `react` `^16.14.0` and `typescript` `~5.1.0` (read 2026-10-16). One
  reason the web client chose React (question 1 of §10 of `../11-surface-web.md`).
- **[V]** OWASP's password storage guidance: "Use Argon2id with a minimum configuration of 19 MiB
  of memory, an iteration count of 2, and 1 degree of parallelism" ([OWASP Password Storage Cheat
  Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html), read
  2026-10-16). The parameters §9.6 of `../08-identity.md` stores passwords with.
- **[V]** The Rust WebAuthn relying-party libraries, as read from crates.io on 2026-10-16:
  `webauthn-rs` and its core are MPL-2.0, outside the permissive licences `deny.toml` admits;
  `webauthn_rp` is MIT or Apache-2.0, published by one maintainer from a personal repository, with
  no release since 2025-04-03 and about 35,000 downloads in all. Why the relying party of §9.6 of
  `../08-identity.md` is Korpis's own code.
- **[V]** OWASP's device cookies slow online guessing without letting a stranger lock an account
  out of its holder's own browsers: a browser that signed in to an account is given a cookie the
  server signs, failures from it are counted against the cookie alone, and failures from browsers
  without one are counted against the account, past a threshold refusing every untrusted browser
  for that account for a while, which OWASP names as a denial an attacker can still cause
  ([OWASP: Slow Down Online Guessing Attacks with Device
  Cookies](https://community.owasp.org/Slow_Down_Online_Guessing_Attacks_with_Device_Cookies),
  read 2026-10-16). Why the login's limits count a target, and the API's credentials do not
  (§10 of `../10-api.md`).
- **[V]** A cookie's path is not a boundary between the pages of one site ("Cookies do not always
  provide isolation by path"), and a host can set a cookie its sibling cannot tell from one it set
  itself ("Cookies do not provide integrity guarantees for sibling domains") ([RFC 6265 §8.5 and
  §8.6](https://www.rfc-editor.org/rfc/rfc6265.html#section-8.5), read 2026-10-17). A cookie whose
  name begins `__Host-` is kept only when it was set with `Secure`, `Path=/`, and no `Domain`
  ([draft-ietf-httpbis-rfc6265bis-22
  §4.1.3.2](https://datatracker.ietf.org/doc/html/draft-ietf-httpbis-rfc6265bis), read
  2026-10-17). Why the session and device cookies of §9.4 and §9.6 of `../08-identity.md` are
  `__Host-` cookies (Finding 72 of `../23-walkthroughs.md`).
- **[V]** OWASP ASVS 4.0.3 requirement 3.7.1, at every level: "Verify the application ensures a
  full, valid login session or requires re-authentication or secondary verification" before
  high-value transactions or changes to account settings ([ASVS V3 Session
  management](https://github.com/OWASP/ASVS/blob/v4.0.3/4.0/en/0x12-V3-Session-management.md),
  read 2026-10-17). Why a change to an account's factors needs a recent sign-in (§9.6 of
  `../08-identity.md`).
- **[V]** RFC 8628, the OAuth device authorization grant: user codes from the twenty consonants
  "BCDFGHJKLMNPQRSTVWXZ", eight of them for an entropy of 20^8 (§6.1); a poll interval of five
  seconds by default (§3.2), which a `slow_down` answer lengthens by five seconds "for this and
  all subsequent requests" (§3.5); "it is recommended that the server rate-limit user code
  attempts" (§5.1); and, against remote phishing, "it is RECOMMENDED to inform the user that they
  are authorizing a device" (§5.4) ([RFC 8628](https://www.rfc-editor.org/rfc/rfc8628.html), read
  2026-10-17). How `korpis login` signs in (§9.4 of `../08-identity.md`).
- **[V]** RFC 7636, PKCE: the verifier is 43 to 128 characters (§4.1) and the `S256` challenge is
  the base64url of its SHA-256 (§4.2) ([RFC 7636](https://www.rfc-editor.org/rfc/rfc7636.html)).
  RFC 9700, the OAuth security best current practice: "Public clients MUST use PKCE" against
  authorization code injection, it is RECOMMENDED for confidential clients, and it "prevents CSRF
  even in the presence of strong attackers" (§2.1.1); the forgery of a redirect into a victim's
  client is §4.7; and under rotation "the authorization server cannot determine which party
  submitted the invalid refresh token", so it revokes the active one too (§4.14.2) ([RFC
  9700](https://www.rfc-editor.org/rfc/rfc9700.html), read 2026-10-20). Why the panel holds the
  verifier of a sign-in through an identity provider, and why the OIDC provider refreshes one
  person at a time (§9.7 of `../08-identity.md`, Finding 119 of `../23-walkthroughs.md`).
- **[V]** OpenID Connect Core 1.0: the ID token validation rules, `iss` that "MUST exactly match",
  `aud` that must contain the client, `azp` the client when present, `exp`, `iat`, and a `nonce`
  that must be present when one was sent (§3.1.3.7); `sub` "MUST NOT exceed 255 ASCII characters"
  (§2), and `sub` and `iss` "used together, are the only Claims that an RP can rely upon" as an
  identifier (§5.7); `offline_access` "requests that an OAuth 2.0 Refresh Token be issued", with
  the person's consent (§11) ([OIDC
  Core](https://openid.net/specs/openid-connect-core-1_0.html), read 2026-10-20). What the OIDC
  provider validates and how it re-verifies (§5.3 and §9.7 of `../08-identity.md`).
- [Nomad: task driver plugins](https://developer.hashicorp.com/nomad/plugins/drivers), [exec2
  driver](https://developer.hashicorp.com/nomad/plugins/drivers/exec2), [qemu
  driver](https://developer.hashicorp.com/nomad/docs/deploy/task-driver/qemu)

- **[V]** OpenSSH's `AuthorizedKeysCommand`, the hook a server uses to look a key up elsewhere,
  is given the key (`%k`), its fingerprint (`%f`), its type (`%t`), and the user, and no token
  carries the client's signature, which `sshd` verifies itself
  ([sshd_config(5)](https://man.openbsd.org/sshd_config), read 2026-10-22). russh 0.64's `Handler`
  likewise calls `auth_publickey` with the user and the public key alone, after it has verified
  the signature ([russh::server::Handler](https://docs.rs/russh/latest/russh/server/trait.Handler.html),
  read 2026-10-22). Why a relay serving SFTP is believed when it says a client holds a key (§3.1
  of `../02-architecture.md`, §9.9 of `../08-identity.md`).
- **[R]** NIST SP 800-57 Part 1 Rev. 5, Tables 2 and 4, as reproduced by secondary sources: RSA
  with a 2,048-bit modulus gives 112 bits of security strength, acceptable for new signatures
  through 2030 and disallowed after; 3,072 bits gives 128, acceptable beyond 2030
  ([Bootlin](https://bootlin.com/blog/the-nists-new-plan-for-digital-signatures-impact-on-secure-boot/),
  [AQtive Guard](https://docs.aqtiveguard.com/kb-articles/cryptographic-keylengths/), read
  2026-10-22; the PDF itself was not read). Why an SSH key of RSA is at least 3,072 bits (§9.9 of
  `../08-identity.md`).
- **[V]** RFC 8441 (WebSocket over HTTP/2): a client may use extended CONNECT once the server
  sends `SETTINGS_ENABLE_CONNECT_PROTOCOL` with a value of 1 (§3); `:protocol` "MUST have a value
  of "websocket" to initiate a WebSocket connection on an HTTP/2 stream" (§5); HTTP/2 has no `101`,
  and the example answers `:status = 200` (§5.1)
  ([RFC 8441](https://www.rfc-editor.org/rfc/rfc8441.html), read 2026-10-22). How the ingress
  carries a WebSocket on HTTP/2 (§3.2 of `../07-networking.md`).
- **[V]** RFC 9440 (Client-Cert HTTP header field): `Client-Cert` is a Byte Sequence of the
  DER-encoded end-entity certificate, `Client-Cert-Chain` a List of them (§2); incoming instances
  "MUST be removed or overwritten before forwarding the request" (§2.4, §4)
  ([RFC 9440](https://www.rfc-editor.org/rfc/rfc9440.html), read 2026-10-22). How the ingress
  tells a workload which certificate a client presented (§3.2 of `../07-networking.md`).
- **[V]** HAProxy's PROXY protocol specification, §2.2 (binary header format, version 2): a
  twelve-byte signature, then `\x21` for version 2 and the command PROXY, then `\x11` for TCP
  over IPv4 or `\x21` for TCP over IPv6, then the address block's length, 12 or 36; the source
  address, the destination address, the source port and the destination port, in that order and
  in network byte order. The header is "prepended before any data flowing from the sender to the
  receiver", and "the receiver SHOULD ensure proper access filtering so that only trusted proxies
  are allowed to use this protocol"
  ([proxy-protocol.txt](https://www.haproxy.org/download/2.9/doc/proxy-protocol.txt), read
  2026-10-22). What the ingress writes to a `passthrough` workload that declared it (§3.2 of
  `../07-networking.md`), checked in its tests by the `ppp` crate's parser.
- **[V]** RFC 9113 §9.1.1: "connection reuse additionally depends on having a certificate that is
  valid for the host in the URI", "a single certificate can be used to establish authority for
  multiple origins", and a server may refuse a request on a connection by "sending a 421
  (Misdirected Request) status code" ([RFC 9113](https://www.rfc-editor.org/rfc/rfc9113.html),
  read 2026-10-22). Why the ingress refuses a request whose route asks for another client
  certificate than its connection's (Finding 137 of `../23-walkthroughs.md`).
- **[V]** RFC 8737 §3 and §4 (TLS-ALPN-01): the validating server "MUST provide an ALPN extension
  with the single protocol name "acme-tls/1"", over a connection that "MUST use TCP port 443";
  the certificate presented carries "a subjectAltName extension containing the dNSName being
  validated and no other entries" and "a critical acmeIdentifier extension" (`id-pe` 31,
  1.3.6.1.5.5.7.1.31) holding the SHA-256 digest of the key authorization as an OCTET STRING of
  32 bytes; the protocol "does not carry application data"
  ([RFC 8737](https://www.rfc-editor.org/rfc/rfc8737.html), read 2026-10-22). How a node's proxy
  answers a validation, by the protocols alone (§3.2 of `../07-networking.md`).
- **[V]** RFC 9113 §6.5.2, `SETTINGS_MAX_CONCURRENT_STREAMS`: "It is recommended that this value be
  no smaller than 100, so as to not unnecessarily limit parallelism"; and §5.1.2, a stream beyond
  the limit is "a stream error of type PROTOCOL_ERROR or REFUSED_STREAM"
  ([RFC 9113](https://www.rfc-editor.org/rfc/rfc9113.html), read 2026-10-22). The ingress's bound
  on the streams one HTTP/2 connection carries (Finding 138 of `../23-walkthroughs.md`).
- **[V]** RFC 9440 §2, §2.1 to §2.3: `Client-Cert` holds "the end-entity certificate used by the
  client in the TLS handshake", and `Client-Cert-Chain` "the certificate chain used for
  validation of the end-entity certificate", which "does not itself include the end-entity
  certificate" and may omit the root; each certificate a Byte Sequence of its DER (§3.3.5 of RFC
  8941, `:base64:`) ([RFC 9440](https://www.rfc-editor.org/rfc/rfc9440.html), read 2026-10-22).
  How the ingress writes them (§3.2 of `../07-networking.md`).
- **[V]** RUSTSEC-2023-0071 (CVE-2023-49092), the Marvin attack on the `rsa` crate: "information
  about the private key is leaked through timing information which is observable over the
  network", with no patched version as of 2026-09-12; the advisory's workaround is to avoid the
  crate where an attacker can observe timing
  ([RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html), read from the
  advisory database `cargo deny` fetches, 2026-10-22). Why a relay's SSH host key is Ed25519 and
  never RSA (§7 of `../21-stack.md`).
- **[V]** RUSTSEC-2026-0344, a decompression bomb in `ruzstd` before 0.9.1: "a crafted `.zst`
  input under 128 KB can force `ruzstd` to allocate several gigabytes at once", since a block's
  decompressed size was never checked against the format's maximum; denial of service only, fixed
  in 0.9.1 ([RUSTSEC-2026-0344](https://rustsec.org/advisories/RUSTSEC-2026-0344), read from the
  advisory database `cargo deny` fetches, 2026-10-24). Why the install job's zstd decoder is
  `ruzstd` 0.9.1 and no earlier (§4 of `../09-recipes.md`).
- **[V]** Linux's ext4 will not mount a filesystem whose journal needs recovery from a read-only
  device, even read-only: it logs "INFO: recovery required on readonly filesystem" and "write
  access unavailable, cannot proceed (try mounting with noload)", while from a writable device
  the same read-only mount logs "write access will be enabled during recovery" and replays the
  journal into it (`ext4_load_journal` in `fs/ext4/super.c`; reproduced on 6.8 with an image
  `debugfs` flagged `needs_recovery`, 2026-10-24). Why a block device's backup is read from a
  staged copy attached writable and mounted read-only, rather than attached read-only (§4.4 of
  `../04-runtimes.md`); `noload` would mount it without the journal's last transactions.
- **[V]** `copy_file_range(2)` between two ext4 filesystems fails with `EXDEV` (Cross-device link,
  os error 18). The manual page as installed: "EXDEV (since Linux 5.19) The files referred to by
  fd_in and fd_out are not on the same filesystem, and the source and target filesystems are not
  of the same type, or do not support cross-filesystem copy"; ext4 is of the same type and does
  not support it. Met 2026-10-24 by the guest's restore, copying from a backup's image to a
  volume. Why a restore's copy reads and writes each data run itself (§4.4 of
  `../04-runtimes.md`).
- **[V]** containerd v2.4.2 (revision b7a25261), when a container's shim dies, logs "cleaning up
  dead shim", runs the shim's `delete` in the task's bundle, kills it after five seconds ("failed
  to delete dead shim" error="signal: killed"; "failed to clean up after shim disconnected"
  error="shim killed after context deadline exceeded"), and leaves the bundle under
  `/run/containerd/io.containerd.runtime.v2.task/<namespace>/<id>`, so a new task of the
  container is refused with "mkdir ...: file exists" until containerd restarts. Seen on every
  try on this machine, 2026-10-24, in containerd's own journal; the same `delete` run by hand in
  the bundle ends at once and removes runc's state. A related race in the same cleanup, leaking
  runc's state, is [containerd#14416](https://github.com/containerd/containerd/issues/14416)
  (**[R]** as a match for this case: it describes a removed bundle, not a timed-out delete). Why
  the `oci` driver finishes the cleanup itself before it starts a task (§4.2 of
  `../04-runtimes.md`).
- **[V]** runc 1.5.2 (commit 29dd3dc2), run by containerd v2.4.2, gives a process added to a
  container (`runc exec --process`) whose spec names no `capabilities` the container's own sets,
  not none: seen 2026-10-24 on this machine, the probe of the driver conformance suite reading
  `CapEff`, `CapPrm`, and `CapBnd` equal to its entry's with the field removed from the `oci`
  driver's exec spec. Why that removal was not kept as the break that shows the suite's `Exec`
  identity case fails (trace 70 of `../23-walkthroughs.md`).
- **[V]** Firecracker v1.17.0 filters its own system calls with seccomp unless it is started with
  `--no-seccomp`, which its help calls "Not recommended", or given another filter with
  `--seccomp-filter` (`firecracker --help` as installed, read 2026-10-25). The `firecracker`
  driver passes neither. Why §5 of `../17-security.md` counts the filters in the `microvm` row as
  built.
- **[V]** The Container Device Interface specification, version 1.1.0
  ([SPEC.md](https://github.com/cncf-tags/container-device-interface/blob/main/SPEC.md), read
  2026-10-25): a JSON file names a vendor's `kind` (`vendor.com/name`) and its devices, each with
  `containerEdits` applied only when it is requested, beside the file's own edits applied when any
  of its devices is; the edits are environment, device nodes (path, host path, type, numbers,
  mode, cgroup permissions, owner), mounts, hooks at an OCI hook point run with an absolute path,
  additional groups, Intel RDT, and network devices moved into the container; loading fails on an
  unknown field. Why a node's devices are read from such files and their edits passed to the
  driver as data (§6.1 of `../05-scheduling.md`, §4.2 of `../04-runtimes.md`).
- **[V]** containerd v2.4.2 sets `cdi_spec_dirs = ['/etc/cdi', '/var/run/cdi']` only under its
  CRI runtime plugin (`containerd config default`, read 2026-10-25 on this machine), so a client
  of containerd's own API, as the `oci` driver is, has no CDI device applied for it. Why the edits
  are applied by the driver (§4.2 of `../04-runtimes.md`).
- **[V]** cgroup v2 cpuset partitions: a partition has exclusive use of its exclusive CPUs, and
  no cgroup outside it can use them; a remote partition is one whose parent is not a partition
  root, every ancestor but the root cgroup must not be one, and the right
  `cpuset.cpus.exclusive` must be written down the hierarchy before the partition root is made
  (`Documentation/admin-guide/cgroup-v2.rst` on the kernel's master branch, read 2026-10-25;
  `cpuset.cpus.exclusive` and `cpuset.cpus.partition` are present on this machine's 6.8 kernel).
  **[R]** that remote partitions shipped in Linux 6.7, from Waiman Long's series of 2023
  ([v8](https://patchew.org/linux/20230905133243.91107-1-longman@redhat.com),
  [LWN](https://lwn.net/Articles/941674)); preflight checks the files, not the version. Why a
  held core's cgroup is a partition of its own (§5.2 of `../05-scheduling.md`).
- **[V]** cgroup v2 refuses a write of `cpuset.cpus.exclusive` that shares a CPU with a sibling's
  exclusive CPUs (the exclusivity rule: one parent's exclusive CPU goes to at most one child), or
  that is a superset of a sibling's `cpuset.cpus`
  (`Documentation/admin-guide/cgroup-v2.rst` on the kernel's master branch, read 2026-10-25). Why
  the helper is told the CPUs held under each driver's cgroup parent apart, shrinks every
  exclusive set before it grows any, and narrows the `shared` cgroup first (§5.2 of
  `../05-scheduling.md`, §3 of `../18-operations.md`).
- **[V]** Firecracker's jailer, under cgroup v2, keeps each `--cgroup file=value` in the order
  given and writes them into the guest's cgroup, enabling each one's controller in every parent,
  before it moves itself in and runs Firecracker (`src/jailer/src/cgroup.rs`, `CgroupV2::
  write_values` and `setup_cgroup_conf`, on Firecracker's main branch, read 2026-10-25; the jailer
  installed here is v1.17.0). Why the `firecracker` driver has the jailer write a guest's `cpuset`
  and then its partition, and reads the partition back rather than trusting the order alone (§4.2
  of `../04-runtimes.md`).
- **[V]** runc, in a container with a user namespace, does not make the device nodes its spec's
  `linux.devices` lists but binds each from the host at the node's own path, the container path
  (`createDevices` and `bindMountDeviceNode` in `libcontainer/rootfs_linux.go` on its main branch,
  read 2026-10-25; runc 1.5.2 is installed here). Why the `oci` driver binds a device's node from
  the host path its CDI edit names rather than listing it there (§4.2 of `../04-runtimes.md`).
- **[V]** CDI's reference implementation, the Go module `tags.cncf.io/container-device-interface`
  (`pkg/cdi/spec-dirs.go`, `cache.go`, and `spec.go` on its main branch, read 2026-10-25), reads
  `/etc/cdi` and then `/var/run/cdi`, each directory's `.json` and `.yaml` files without
  recursing, a directory that does not exist skipped; a device named in a later directory
  replaces one of the same name in an earlier, two of one name in one directory are a conflict
  and neither is kept, and a file is parsed with unknown fields refused (`KnownFields(true)`). Why
  the agent orders and refuses as it does (§6.1 of `../05-scheduling.md`); the one difference,
  that a conflict in an earlier directory is settled by a later one naming the device rather than
  kept, is the agent's.
- **[V]** NVIDIA's Container Toolkit, from v1.18.0, generates its CDI file at
  `/var/run/cdi/nvidia.yaml` by a systemd service, `nvidia-cdi-refresh`, when the toolkit or the
  GPU driver is installed or upgraded, and not when MIG partitions are reconfigured; the file
  names `nvidia.com/gpu=all`, each GPU by index (`nvidia.com/gpu=0`), and each MIG device as
  `nvidia.com/gpu=1:0`, a colon the CDI specification's grammar for a device's name does not list
  and which Korpis therefore accepts between ([CDI support](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/cdi-support.html),
  read 2026-10-25). A MIG device's own edits give its parent GPU's node and the capability nodes
  of its GPU instance and its compute instance (`newNvmlMigDiscoverer` in
  `internal/platform-support/dgpu/nvml.go` of the toolkit's main branch, read 2026-10-25), so its
  nodes contain the GPU's and no other partition's. Why §6.1 of `../05-scheduling.md` treats a
  device whose nodes contain another's as one hardware and an aggregate as not offered.
- **[R]** CVE-2024-0132: NVIDIA's Container Toolkit up to v1.16.1, in its default configuration,
  had a time-of-check, time-of-use flaw letting a crafted image escape its container to the host;
  the use of CDI was reported not affected
  ([The Register](https://www.theregister.com/2024/09/26/critical_nvidia_bug_container_escape/),
  [Wiz](https://www.wiz.io/blog/wiz-research-critical-nvidia-ai-vulnerability)). NVIDIA's
  bulletin could not be read from here, hence [R]. Why §5 of `../17-security.md` counts a
  device's toolkit among the host code a device brings.
- **[V]** OpenSSH 9.6's client offers, by default, RSA signatures only as `rsa-sha2-512` and
  `rsa-sha2-256`, never `ssh-rsa` (`PubkeyAcceptedAlgorithms` in ssh_config(5) as installed,
  read 2026-10-22). Why a relay accepting an `ssh-rsa` signature that `russh` does not refuse is a
  cost stated rather than one met in practice (§7 of `../21-stack.md`).
- **[V]** Firecracker on x86 does not virtualize power management: a guest's `poweroff` leaves the
  Firecracker process running, while a `reboot` under `reboot=k` resets the CPU through the
  emulated i8042 keyboard controller, which Firecracker intercepts and exits on; its
  `SendCtrlAltDel` action injects the key sequence through the same controller, and needs
  `CONFIG_SERIO_I8042` and `CONFIG_KEYBOARD_ATKBD` in the guest
  ([FAQ](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/FAQ.md),
  [actions](https://github.com/firecracker-microvm/firecracker/tree/v1.17.0/docs/api_requests),
  read 2026-10-23 at v1.17.0). Why a microVM's run ends with its guest restarting, and a stop
  arrives as Ctrl+Alt+Del (§6 of `../04-runtimes.md`).
- **[V]** The OCI image layer specification, "Whiteouts": a whiteout is an empty file named `.wh.`
  and the basename of the path deleted, which "MUST only apply to resources in lower/parent
  layers", so "files that are present in the same layer as a whiteout file can only be hidden by
  whiteout files in subsequent layers"; an opaque whiteout, `.wh..wh..opq`, hides all of its
  directory's children in the lower layers
  ([layer.md](https://github.com/opencontainers/image-spec/blob/v1.1.1/layer.md), read
  2026-10-23 at v1.1.1). How a microVM's image drive applies layers (§6 of `../04-runtimes.md`).
- **[V]** WireGuard's cryptokey routing: each peer is identified by its public key and has a list
  of allowed source IPs; an outgoing packet is encrypted for the peer whose allowed IPs hold its
  destination, and a decrypted incoming packet is accepted only if its source resolves to the key
  that decrypted it, otherwise it is dropped, so a packet arriving on the interface has an
  authentic source address (whitepaper §2). A responder sends nothing to an unauthenticated
  message and keeps no state for one (§5.1). A secure session is renewed every Rekey-After-Time,
  120 seconds, and one older than Reject-After-Time, 180 seconds, carries no data (§6.1)
  ([WireGuard: Next Generation Kernel Network
  Tunnel](https://www.wireguard.com/papers/wireguard.pdf), read 2026-10-25). Why a mesh packet's
  source names its node, why the mesh's port answers no scanner, and why a pair is judged by a
  handshake younger than 180 seconds (§9.1 of `../07-networking.md`).
- **[V]** `wg(8)`: a peer's endpoint is updated to the source of its most recent authenticated
  packet, so a peer behind NAT is answered where it last spoke from; `PersistentKeepalive` sends
  an authenticated empty packet at an interval to keep a NAT mapping valid; `wg show` reports each
  peer's latest handshake. `wg-quick` sets an interface's MTU to its route's MTU less 80
  ([wg.8](https://git.zx2c4.com/wireguard-tools/plain/src/man/wg.8),
  [linux.bash](https://git.zx2c4.com/wireguard-tools/plain/src/wg-quick/linux.bash), read
  2026-10-25). WireGuard is in the Linux kernel from 5.6
  ([install](https://www.wireguard.com/install/), which offers modules only to users of older
  kernels). The mesh's NAT, MTU, and kernel floor (§9.1 of `../07-networking.md`).
- **[V]** Proxmox VE storage replication sends a guest's ZFS volumes to another node on a
  schedule of one minute at the shortest and a week at the longest, the first copy whole and each
  later one as the difference between two snapshots; a guest started from a replica after its node
  fails has what the last run sent, and the documentation has its operator start it there only
  after making sure the original node no longer runs it
  ([Storage Replication](https://pve.proxmox.com/wiki/Storage_Replication),
  [pvesr](https://pve.proxmox.com/pve-docs-7/chapter-pvesr.html), read 2026-10-27). Replicated
  volumes, their interval, and their stated loss (§2.2 of `../06-storage.md`).
- **[V]** CephFS quotas rely on the cooperation of the client mounting the filesystem, which stops
  writers some time after the limit is crossed, generally within tens of seconds, and a modified
  client can ignore them ([CephFS Quotas](https://docs.ceph.com/en/reef/cephfs/quota/), read
  2026-10-27). Why a shared class declares no quota (§2.1 of `../06-storage.md`).
- **[V]** An NFSv4 client renews its lease with the server by its own requests, and a server
  releases a client's state when that lease expires (RFC 7530 §9.5 and §9.6). The lease is the
  server's view of the client, not anyone else's: Finding 198 of `../23-walkthroughs.md`, which
  removed it from the backends that refuse a superseded writer.
- **[V]** `open(2)`: a program that needs an atomic lock file on NFS creates a unique file on the
  same filesystem and links it to the lock file's name; the lock is held if `link(2)` returns 0, or
  if the unique file's link count is then 2 (the man page's note on `O_EXCL`). How the first
  helper on a shared store writes its identity (§2.1 of `../06-storage.md`).
Secondary:
- [TheGamePanel: a modern alternative to
  Pterodactyl](https://ollieread.com/articles/thegamepanel-a-modern-alternative-to-pterodactyl)
- [Pyrodactyl](https://github.com/pyrohost/pyrodactyl) · [Our journey and the future of
  Pyrodactyl](https://pyro.engineering/posts/our-journey-and-the-future-of-pyrodactyl/)
- [Pelican FAQ](https://pelican.dev/faq/)
- [Pterodactyl is back and keeps its MIT
  license](https://teramont.net/blog/pterodactyl-is-back-and-keeps-its-mit-open-source-license)
- [AMP vs Pterodactyl (CubeCoders)](https://cubecoders.com/compare/pterodactyl)
- [Coolify vs
  Dokploy](https://servercompass.app/blog/coolify-vs-dokploy-self-hosted-paas-comparison)
- [Paymenter vs WHMCS vs Blesta](https://builtbyotte.com/blog/paymenter-vs-whmcs-vs-blesta)
