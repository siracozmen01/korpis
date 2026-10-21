# Korpis

**Status:** design **Date:** 2026-08-07

---

## 1. What Korpis is

Korpis is an open-source platform for running workloads on machines you control.

A workload is anything with a lifecycle: a game server, a Discord bot, a web service, a database, a
scheduled job, a virtual machine. Korpis places it on a node, isolates it to the degree you
specify, gives it storage and network, keeps it in the state you declared, and lets you and the
people you delegate to operate it, from a web panel, from Discord, from a terminal, or from an API.

It is aimed at the people Pterodactyl serves today and the people it turns away: communities
running shared infrastructure, teams running their own services, and hosting providers selling
capacity to customers.

---

## 2. What Korpis is not

Pterodactyl never wrote this section. Its v2 rewrite was announced and abandoned, and three
independent teams have since concluded its architecture cannot be repaired in place. A project of
this size fails by accretion, not by any single wrong decision. So the boundary is drawn first.

**Korpis is not a billing system.** It will never contain invoices, payment gateways, tax handling,
dunning, or a customer-facing store. It publishes what billing systems need and never get
(organizations, quotas, metering, lifecycle events, SSO), so that an integration is a few hundred
lines and requires no fork. (Rule K-12.)

**Korpis is not a game-specific product.** No game appears in the core. Game-awareness (query
protocols, RCON, mod installation, map rotation, wipe scheduling) lives entirely in recipes and
extensions. If a change to core is needed to support a game, the extension interface is wrong.

**Korpis is not a Kubernetes distribution and does not run on one.** It borrows the control-loop
model and Agones' scheduling vocabulary. It does not borrow Kubernetes' operational cost. A single
operator with one machine must be able to install and run Korpis.

**Korpis is not a DNS, email, or CDN provider.** It consumes DNS through a provider interface for
ingress and certificates. It does not become one.

**Korpis is not a hypervisor, container runtime, or filesystem.** It drives containerd, QEMU/KVM,
Firecracker, ZFS, and btrfs. It reimplements none of them.

**Korpis is not an operating system.** It installs onto Linux, and eventually Windows, that someone
else maintains.

**Korpis is not a monitoring stack.** It exports metrics, logs, traces, and events in open formats
and integrates with Prometheus and OpenTelemetry. It does not build dashboards, alert routing, or
long-term metric storage.

**Korpis is not a marketplace.** Recipes are distributed through OCI registries, which already
provide hosting, mirroring, signing, and CDN. Korpis does not operate a store, take a cut, or
curate.

**Korpis does not have a free tier.** Korpis is free. There is no edition matrix, no instance cap,
no feature gated behind a license, and no commercial-use restriction. (Rule K-13.)

---

## 3. Principles

P1 to P9 are ordered: when two conflict, the higher number yields. P10 is outside the ordering. It
constrains what Korpis may charge for or forbid, not how a design trade-off is decided, so it is
never weighed against the others and never yields to them.

> Corrected 2026-10-12. This said "These are ordered", which made P10 the principle that yields to
> every other, the opposite of how every document applies it.

### P1. Truth is declared, not commanded

You declare what should be true. Korpis converges reality toward it, continuously, and reports what
is actually true. No component tells another component to perform an action and assumes it
happened.

*Beats:* convenience, latency, implementation simplicity. *Because:* Pterodactyl's "the panel says
stopped but the server is running" class of bug is not a bug. It is the inevitable outcome of
imperative RPC with no reconciliation.

### P2. The control plane is not the product; the model is

Web, CLI, chat, and API are peer clients of one API. None is privileged. None can do something
another cannot. The panel is a client of Korpis, not Korpis itself.

**Peer does not mean in-core.** A first-party extension is a peer client if it can do everything a
built-in one could; if it cannot, the extension contract is the thing that is broken (P8). The rule
constrains *capability*, never which repository the code lives in.

*Beats:* every argument that begins "but it would be faster to special-case the web UI". *Because:*
a surface that is architecturally second-class is permanently second-class. Discord bots for every
existing panel consume webhooks; they notify, they do not govern.

### P3. Confine with the kernel, not with strings

Privilege is separated by process and confined by kernel mechanisms, namespaces, cgroups,
capabilities, seccomp, Landlock, `openat2(RESOLVE_BENEATH)`. Validation of paths, names, and inputs
is defence in depth, never the defence.

*Beats:* performance, code simplicity, developer convenience. *Because:* the largest class of
published Wings advisories, six of twenty-one by October 2026, is one shape: a privileged process
defending tenant-supplied paths by inspecting strings. Symlinks, races, and `..` win that fight
every time.

### P4. Never claim what is not enforced

Every limit Korpis displays is enforced by the kernel and metered. Every status Korpis reports is
observed, not assumed. When Korpis cannot observe something, it says "unknown" rather than
guessing.

*Beats:* feature-list completeness, UI tidiness. *Because:* Pterodactyl displays a disk quota it
does not enforce, and closed the report of an 858 GB file on a 1 GB limit as *not planned*. A limit
that is not enforced is a lie in a text field.

### P5. Nothing happens that could not have been inspected first

Every change produces a Plan before it produces an Effect. The Plan is a real, persisted,
inspectable object. Dry-run is not a mode; it is the first half of every operation.

*Beats:* the number of clicks required to do something simple. *Because:* this is what makes
approval workflows, scheduled changes, rollback, and honest audit possible, and none of them can be
retrofitted onto a system that mutates directly.

### P6. Authority is delegated and attenuated, never assigned

There are no roles. There is one primitive: a grant, which carries a subject, actions, a scope, and
conditions, and which can only ever produce weaker children. Administrator is a grant with a broad
scope, not a special case in the code.

*Beats:* familiarity. Everyone expects RBAC. *Because:* RBAC cannot express "this link lets your
friend restart this one server for the next 24 hours, with no account", and that is the operation a
chat-native platform performs constantly.

### P7. Shape is declared, not assumed

Lifecycle, interaction, endpoints, storage, health, and isolation are **independent declared
fields**, not points on a single enum (§2 of `01-model.md`). A long-running console process, an
HTTP service, a one-shot job, a scheduled task, and a full virtual machine are combinations of the
same fields, sharing one scheduler, one storage layer, one network layer, one identity model.
Adapters differ; the core does not.

*Beats:* the desire to ship one shape well and add the rest later. *Because:* Pterodactyl assumed a
long-running process with a console. Coolify assumed an HTTP service behind a proxy. Both froze
that assumption into the schema, and neither can reach the other.

### P8. Extensions are peers, not patches

Extensions run as separate processes, hold narrowly-scoped grants, and speak a versioned contract.
They cannot modify Korpis' own files. Core features use the same extension mechanism third parties
use, if it is not sufficient for core, it does not ship.

*Beats:* the extra work of designing an interface before there are consumers for it. *Because:*
Pterodactyl has no extension API, so the ecosystem standardized on `sed`-patching core files, and
breaks on every upgrade.

### P9. Reversibility over speed

Every operation that changes durable state is resumable, verifiable, and reversible. No operation
may leave a workload in a state the model cannot represent.

*Beats:* single-shot implementations that are simpler when they succeed. *Because:* Pterodactyl's
transfer is a single-shot imperative operation, so an interrupted transfer deadlocks the server
into a state requiring manual SQL to escape, and has done so, in issue after issue, for years.

### P10. Never tax growth

No per-workload, per-node, per-instance, per-account, or per-domain charge. Ever. No feature is
gated by edition. No use is forbidden by license.

*Beats:* every business model in this market. *Because:* the commercial products here charge per
game server, per machine, per instance, per account, or per domain, and several restrict commercial
reselling below their top tier. Every one of those models taxes exactly the thing a growing host is
trying to do. Pterodactyl removed the tax and took the market; that is the only durable advantage a
new entrant has, and imposing a tax forfeits it.

---

## 4. The four bets

Principles say how to decide. These are the specific, falsifiable claims Korpis is making about the
market, the places it deliberately departs from what everyone else does.

**Bet 1: Authority without accounts.** Every product in this market assumes a user is a row in its
own users table, so giving someone limited, temporary access means provisioning them first. Korpis
makes authority a delegable, attenuable grant that can be issued to a link, to a chat identity, or
to an external role, with no account created anywhere. The wager is that this (not the panel) is
what the market actually lacks, and that once it exists, every other delegation case falls out of
it: resellers, SSO, CI pipelines, support access, and a moderator who restarts one server from a
phone.

Chat is the sharpest test of the claim, not the claim itself. A chat client can only govern if
authority already flows this way; every Pterodactyl Discord bot consumes webhooks (it can tell you
something happened, it cannot govern), and that is a symptom of the missing model, not of a missing
bot.

**Bet 2: Isolation is an operator choice, not a platform property.** Pterodactyl mandates
containers. PufferPanel and GameAP forbid them. AMP makes them optional but ungraded. Nobody lets
an operator say *this tenant is untrusted, give it a microVM; this one is mine, give it a confined
process.* Korpis makes isolation strength a per-workload field backed by a runtime driver
interface. The wager is that this single decision unlocks Windows, untrusted code, GPU workloads,
and full VMs, every market segment closed to Pterodactyl and every fork of it.

**Bet 3: The diff is a first-class object.** Kubernetes computes the difference between desired and
observed state internally and invisibly. Korpis persists it as an Intent → Plan → Effect chain. The
wager is that making the diff inspectable yields dry-run, approval workflows, scheduled changes,
rollback-as-reapply, and truthful audit as consequences of one decision rather than five features,
and that this is what makes a chat surface safe enough to grant real authority to.

**Bet 4: Correct tenancy produces reselling for free.** TCAdmin sells reseller support as a premium
feature because its tenancy model was not built for it. Korpis models tenancy as recursive
delegation under attenuable grants and inherited quotas. A reseller is a tenant who delegates to
sub-tenants. The wager is that no reseller feature ever needs to be written.

---

## 5. Openness

Korpis is open source. This constrains the licensing decision more than it may appear, and the
evidence points in two opposite directions:

- Pterodactyl chose **MIT**. Closed commercial forks took the work, sold it, and returned nothing.
- Pelican chose **AGPLv3**. Commercial hosts relying on proprietary differentiation left.
- Convoy chose the **Business Source License**: public source, commercial use charged, automatic
  conversion to open source on a schedule. **This option is now closed to Korpis: BSL is
  source-available, not open source.**

**Decision: split the licensing by layer rather than pick one pole.**

| Layer | License | Reasoning |
|---|---|---|
| Protocol definitions, client SDKs, recipe format, agent protocol | **Apache-2.0** | These must be implementable by anyone, including commercial products, with no friction. Rule K-7 requires that a third party be able to write a conforming agent; a copyleft protocol definition would undermine that. Apache-2.0 also carries an explicit patent grant. |
| Control plane, node agent, relay, web client, chat clients, CLI | **AGPL-3.0-or-later** | The platform itself cannot be taken closed. Hosting Korpis for others is exactly the case AGPL exists to cover, and it is the case Pterodactyl lost to. |
| The documents in this repository | **CC BY 4.0** | The specification is the work until code exists, and it should be quotable and buildable-on by anyone, with attribution. |
| Recipes, extensions, themes | **Author's choice** | Extensions are separate processes speaking a published contract (P8), not derivative works. Anything else would poison the ecosystem before it exists. |

The AGPL objection is real: it is what drove commercial hosts away from Pelican. The counter is
that those hosts were differentiating on modifications they never intended to return, and P10 means
Korpis is not competing for them on price; it is free at every scale, forever. A host that wants
proprietary differentiation can build it as an extension under any license it likes, which is
precisely what P8 exists to permit.

**What this means for a hosting provider, stated plainly**, because AGPL is widely misunderstood as
a restriction on use: run Korpis commercially, sell capacity on it, charge whatever you like, build
proprietary extensions against it. Running it unmodified for your customers obliges nothing. Two
things do create obligations: **modifying Korpis itself** and offering the modified version over a
network, in which case you offer its users the source of your version; and **conveying copies**,
binaries, the CLI, or the web client's bundle, in which case the corresponding source goes with
them, which an unmodified build already arranges (§2 of `19-governance.md`). The single thing AGPL
forecloses is taking Korpis, closing it, and selling it as a proprietary product, which is
precisely what happened to Pterodactyl under MIT. What the licence does not foreclose, and what
only policy does, is stated in the same section of `19-governance.md`.

> Corrected 2026-10-12. This said "using it unmodified obliges nothing", which missed the
> obligations attached to conveying copies.

The remaining terms, versioning policy, compatibility guarantees, LTS, trademark policy, and
maintainership and succession, are settled in `19-governance.md` before the first release, per Rule
K-8. Governance was Pterodactyl's actual failure; it is not left until later here.

---

## 6. Inherited constraints

Eighteen rules bind every document that follows. K-1 to K-12 are derived from evidence in
[`research/evidence.md`](./research/evidence.md), each traceable to a specific published advisory,
issue, or product behaviour. K-13 to K-18 are policy and design decisions made while the design was
written; §4 of that file states what each rests on.

> Corrected 2026-10-12. This said nineteen rules, all derived from evidence. K-13 to K-18 are not,
> and K-19 restated K-8 and is retired.

| # | Rule |
|---|---|
| K-1 | Tenant filesystem access is confined by the kernel, never by path-string validation |
| K-2 | Tenant templates evaluate in a sandbox with an allow-listed variable set |
| K-3 | Every displayed limit is kernel-enforced and metered, or it is not displayed |
| K-4 | Migration is a resumable, verifiable, reversible job with explicit phases |
| K-5 | Backup is a consequence of the storage design, not a feature on top of it |
| K-6 | Extensions are out-of-process, versioned, sandboxed; core uses the same mechanism |
| K-7 | Control plane and data plane are separated by a published, versioned protocol |
| K-8 | License, versioning, compatibility, and maintainership are day-one decisions |
| K-9 | The runtime is an interface; it must be able to express Windows on day one |
| K-10 | Isolation strength is a per-workload operator choice |
| K-11 | Workload shape is a field, not an assumption |
| K-12 | Korpis implements what billing needs, not billing |
| K-13 | Never price per workload, node, instance, account, or domain |
| K-14 | Tenancy is recursive delegation; reselling is an emergent property |
| K-15 | Recipes are content-addressed and fetched once per node, not once per workload |
| K-16 | Health is application-level, not process-level |
| K-17 | Configuration is schema-driven, with per-field permissions |
| K-18 | Ship one excellent runtime driver before five adequate ones |
| ~~K-19~~ | Retired 2026-10-12 as a restatement of K-8 |

---

## 7. Document map

| Document | Contents |
|---|---|
| `00-overview.md` | This document |
| `01-model.md` | Core domain model, the nouns, their fields, relationships, lifecycles |
| `02-architecture.md` | Control plane / data plane split, reconciliation protocol |
| `03-state.md` | Intent, Plan, Effect; the event log; storage engine |
| `04-runtimes.md` | Runtime driver interface; OCI, native, Windows, microVM, KVM |
| `05-scheduling.md` | Placement policy, bin-packing, cordon/drain, migration |
| `06-storage.md` | Volumes, snapshots, quotas, content-addressed backup |
| `07-networking.md` | Endpoints, overlay, ingress, TLS, egress policy, shaping, accounting |
| `08-identity.md` | Tenancy, grants, delegation, external identity (Discord, OIDC) |
| `09-recipes.md` | Package format, install DSL, registry, signing, lockfiles |
| `10-api.md` | Schema, transport, versioning, compatibility guarantees |
| `11-surface-web.md` | Web client |
| `12-surface-discord.md` | Discord client |
| `13-surface-cli.md` | CLI and GitOps |
| `14-streams.md` | Durable console, logs, replay, sharing |
| `15-observability.md` | Metrics, metering, audit, alerting |
| `16-extensions.md` | Plugin contract, events, UI slots |
| `17-security.md` | Threat model, isolation tiers, privilege separation |
| `18-operations.md` | Install, upgrade, HA, disaster recovery |
| `19-governance.md` | License, versioning, LTS, contribution, maintainership |
| `20-roadmap.md` | Build order and dependency graph |
| `21-stack.md` | Implementation language: Rust throughout, TypeScript in the browser, and why |
| `22-first-party.md` | The first-party recipe and extension set, and what it proves |
| `23-walkthroughs.md` | Forty-four end-to-end traces and the defects they found; two external reviews |
