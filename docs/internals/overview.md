# Overview

Korpis is an open-source platform for running workloads on machines their operators control. A
workload is anything with a lifecycle: a game server, a chat bot, a web service, a database, a
scheduled job, a virtual machine. Korpis places it on a node, isolates it as strongly as its
operator chooses, gives it storage and network, keeps it in the state that was declared, and lets
its owners and the people they delegate to operate it from a web panel, from Discord, from a
terminal or through the API.

It serves the communities, teams and hosting providers that Pterodactyl serves today, and the
ones it turns away: untrusted tenants, Windows, full virtual machines, resellers.

## Contents

- [How the parts fit](#how-the-parts-fit)
- [What Korpis is not](#what-korpis-is-not)
- [Principles](#principles)
- [Bets](#bets)
- [Rules every part follows](#rules-every-part-follows)

## How the parts fit

```
clients        web panel, Discord, CLI, API consumers, extensions: peers of one API
control plane  korpis-server and PostgreSQL: owns intent, plans, grants, placement, leases
data plane     one agent per node: owns observations, runtime objects, volumes, streams
```

The control plane decides and never commands. Each node's agent dials out to it, holds one
long-lived control stream, receives its assignment (the workloads it should run, at which epoch
of their leases), converges the node toward it, and reports what it observes. Nothing ever
connects to a node, so a node needs no public address and no inbound port. Bulk data (consoles,
files, stream segments) travels on a separate connection to a relay, so a transfer never delays a
lease renewal. Every object has exactly one writer: the control plane never writes an
observation, and the agent never writes an intent.

On a node, the agent is several executables. The supervisor holds the connections and runs the
reconciliation loop as an unprivileged user. A small root helper performs a fixed set of verbs
(mount, cgroup writes, network namespaces, volumes, snapshots, jobs) and accepts no tenant
paths. Runtime drivers (OCI containers, Firecracker microVMs) start workloads and validate every
spec they receive. A filesystem worker per workload serves the file manager and SFTP from a
mount namespace that holds only that workload's volumes.

The pages under this directory describe each part: [architecture](architecture.md) for how the
code is layered, and one page per subject for how that part works and why.

## What Korpis is not

The boundary is drawn first, because a project of this size fails by accretion rather than by
one wrong decision.

- **Not a billing system.** No invoices, payment gateways, taxes or storefront. It publishes what
  billing systems need (organizations, quotas, metering, lifecycle events, single sign-on), so an
  integration is a few hundred lines and no fork.
- **Not game-specific.** No game appears in the core. Query protocols, RCON, mods, map rotation
  and wipes live in recipes and extensions; a game that needs a core change shows the extension
  interface is wrong.
- **Not a Kubernetes distribution, and not run on one.** It borrows the control-loop model and
  Agones' scheduling vocabulary, not Kubernetes' operational cost. One operator with one machine
  installs and runs it.
- **Not a DNS, email or CDN provider.** It consumes DNS through a provider interface.
- **Not a hypervisor, container runtime or filesystem.** It drives containerd, Firecracker,
  QEMU/KVM, btrfs and ZFS, and reimplements none of them.
- **Not an operating system.** It installs onto a Linux someone else maintains.
- **Not a monitoring stack.** It exports metrics, logs, traces and events in open formats for
  Prometheus and OpenTelemetry; it builds no dashboards, alert routing or long-term storage.
- **Not a marketplace.** Recipes are distributed through OCI registries, which already host,
  mirror and sign. Korpis runs no store, takes no cut and curates nothing.
- **No free tier, because it is free.** No edition matrix, instance cap, licence-gated feature or
  commercial-use restriction.

## Principles

When two of the first nine conflict, the later one yields. The tenth is outside that order: it
constrains what Korpis may charge for or forbid, not how a design trade-off is decided, so it is
never weighed against the others.

1. **Truth is declared, not commanded.** Clients declare what should be true; Korpis converges
   toward it continuously and reports what is actually true. No component tells another to act
   and assumes it happened. Pterodactyl's "the panel says stopped but the server is running" is
   not a bug there; it is what imperative calls without reconciliation produce.
2. **The model is the product, not the control plane.** Web, CLI, chat and API are peer clients
   of one API, and none can do what another cannot. A first-party extension is a peer if it can
   do everything a built-in client can; where it cannot, the extension contract is what is broken.
3. **Confine with the kernel, not with strings.** Privilege is separated by process and confined
   by namespaces, cgroups, capabilities, seccomp, Landlock and `openat2(RESOLVE_BENEATH)`.
   Validating paths and names is defense in depth, never the defense: the largest class of
   published Wings advisories is a privileged process defending tenant paths by inspecting
   strings, and symlinks, races and `..` win that fight.
4. **Never claim what is not enforced.** Every limit shown is enforced by the kernel and metered;
   every status shown is observed. What cannot be observed is shown as unknown. Pterodactyl shows
   a disk quota it does not enforce, and closed the report of an 858 GB file on a 1 GB limit as
   not planned.
5. **Nothing happens that could not have been inspected first.** Every change produces a
   persisted, inspectable plan before it produces an effect. Dry-run is the first half of every
   operation, which is what makes approval, scheduled changes, rollback and honest audit possible.
6. **Authority is delegated and attenuated, never assigned.** There are no roles. A grant carries
   a subject, actions, a scope and conditions, and can only produce weaker children. An
   administrator is a grant with a broad scope. Roles cannot express "this link lets your friend
   restart this one server for 24 hours, with no account", which a chat-native platform does
   constantly.
7. **Shape is declared, not assumed.** Lifecycle, interaction, endpoints, storage, health and
   isolation are independent fields, not points on one enum. A console process, an HTTP service, a
   job, a scheduled task and a virtual machine are combinations of the same fields over one
   scheduler, one storage layer, one network layer and one identity model.
8. **Extensions are peers, not patches.** Extensions run as separate processes with narrow grants
   and speak a versioned contract; they cannot modify Korpis's files. Core features use the same
   mechanism. Pterodactyl has no extension API, so its ecosystem patches core files with `sed` and
   breaks on every upgrade.
9. **Reversibility over speed.** Every operation that changes durable state is resumable,
   verifiable and reversible, and none leaves a workload in a state the model cannot represent.
   Pterodactyl's single-shot transfer deadlocks a server into a state that needs manual SQL to
   leave.
10. **Never tax growth.** No per-workload, per-node, per-instance, per-account or per-domain
    charge, limit or gate, at any scale. No feature is gated by edition and no use is forbidden by
    licence. Every commercial product in this market taxes exactly what a growing host is trying
    to do; removing that tax is what took Pterodactyl its market.

## Bets

These are the falsifiable claims where Korpis departs from what everyone else does.

**Authority without accounts.** Every product here assumes a user is a row in its own table, so
limited, temporary access means provisioning someone first. Korpis makes authority a delegable,
attenuable grant issued to a link, a chat identity or an external role, with no account made.
Resellers, single sign-on, CI pipelines, support access and a moderator restarting one server from
a phone all follow from it. Chat is the sharpest test: every Pterodactyl Discord bot consumes
webhooks; it can report, it cannot govern.

**Isolation is the operator's choice.** Pterodactyl mandates containers, PufferPanel and GameAP
forbid them, AMP makes them optional and ungraded. Korpis makes isolation strength a per-workload
field over a runtime driver interface, which opens Windows, untrusted code, GPUs and full virtual
machines.

**The diff is a first-class object.** Kubernetes computes the difference between desired and
observed state invisibly. Korpis persists it as intent, plan and effect, so dry-run, approval,
scheduled changes, rollback by reapplying and truthful audit are consequences of one decision, and
a chat surface becomes safe enough to hold real authority.

**Correct tenancy produces reselling.** TCAdmin sells reseller support as a premium feature
because its tenancy was not built for it. Korpis models tenancy as recursive delegation under
attenuable grants and inherited quotas: a reseller is a tenant that delegates to sub-tenants, and
no reseller feature is ever written.

## Rules every part follows

Each rests on a published advisory, issue or product behavior cited on the page that applies it.

- Tenant filesystem access is confined by the kernel, never by validating path strings.
- Tenant templates evaluate in a sandbox with an allow-listed set of variables.
- Every displayed limit is enforced by the kernel and metered, or it is not displayed.
- Migration is a resumable, verifiable, reversible job with explicit phases.
- Backup is a consequence of the storage design, not a feature on top of it.
- Extensions are out of process, versioned and sandboxed, and the core uses the same mechanism.
- The control plane and data plane are separated by a published, versioned protocol, so a third
  party can write a conforming agent without reading Korpis's source.
- Licence, versioning, compatibility and maintainership are decided before the first release.
- The runtime is an interface that can express Windows.
- Isolation strength is a per-workload operator choice.
- Workload shape is a field, not an assumption.
- Korpis implements what billing needs, not billing.
- Tenancy is recursive delegation; reselling falls out of it.
- Recipes are content-addressed and fetched once per node, not once per workload.
- Health is judged at the application level, not by whether a process exists.
- Configuration is schema-driven, with permissions per field.
- One excellent runtime driver ships before five adequate ones.
