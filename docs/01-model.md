# Core Domain Model

**Status:** design **Date:** 2026-08-07 **Depends on:** [`00-overview.md`](./00-overview.md)

---

## 1. Why this document comes first

Pterodactyl's central noun is `Server`, and `Server` means *a game server*. Every table, endpoint,
permission, and UI screen is built around that word. The reason no fork has been able to make
Pterodactyl general-purpose is not that nobody tried; it is that the vocabulary does not permit it.

Twenty-two documents follow this one. If the nouns here are wrong, all of them are wrong.

---

## 2. A correction to the earlier proposal

An earlier draft proposed a single field:

```
shape: console | service | job | cron | stateful | machine
```

Working it through shows this conflates four independent axes:

- `stateful` is not a shape. **Any** workload may have durable volumes, a game server, a database,
  and a nightly job can all be stateful. Statefulness is a property, not a category.
- `machine` is not a shape. A virtual machine is a **runtime driver choice**, the same persistent
  lifecycle, confined by a hypervisor instead of a kernel namespace. Making it a shape would put
  the isolation decision in the wrong field and quietly contradict Bet 2.
- A Minecraft server is simultaneously *console-interactive* and *a network service*. A single enum
  forces a false choice.
- A Discord bot is long-running with no endpoints and no interactive console. The enum has no
  correct value for it.

The correct model separates the axes:

| Axis | Field | Values |
|---|---|---|
| Lifecycle: how the scheduler treats it | `lifecycle` | `persistent` · `task` · `scheduled` |
| Interaction: what the stream surface offers | `console` | `none` · `logs` · `interactive` |
| Exposure: how the network reaches it | `endpoints[]` | declared list |
| State: what survives a restart | `volumes[]` | declared list |
| Liveness: what "healthy" means | `health` | probe definition |
| Confinement: how strongly it is isolated | `runtime` | driver + isolation tier |

"Shape" survives as an informal word for a **named preset over these fields**, published by a
recipe. It is not a field on the workload.

What this buys, concretely:

| Workload | lifecycle | console | endpoints | volumes | health | runtime |
|---|---|---|---|---|---|---|
| Minecraft server | persistent | interactive | game/25565 | data | query: minecraft | oci / container |
| Next.js app | persistent | logs | http/3000 + ingress | none | http: /healthz | oci / container |
| PostgreSQL | persistent | logs | tcp/5432 | data | tcp + query | oci / container |
| Discord bot | persistent | logs | none | none | heartbeat | oci / container |
| Nightly backup | scheduled | logs | none | archive | exit: 0 | oci / container |
| Customer VPS | persistent | interactive (serial) | tcp/22, ip | root disk | agent or tcp | kvm / vm |
| Untrusted plugin build | task | logs | none | workspace | exit: 0 | firecracker / microvm |

One scheduler, one storage layer, one network layer, one identity model, one API. A VM is not a
special case in the code, it is `runtime.driver: kvm`. That is Bet 2 made structural rather than
aspirational.

---

## 3. Object catalogue

Every object below has: a **purpose** (one sentence), **fields**, **invariants** (what must always
be true), and where relevant a **lifecycle**.

Field types are indicative, not a schema. The normative schema lives in `10-api.md`.

> Audited 2026-10-13 against the wire schema (`crates/korpis-proto`), object by object, because
> rule E of `20-roadmap.md` freezes every field a later phase needs in Phase 0. Two fields this
> catalogue promised were missing from the schema, `Placement.reserved` and `Stream.segments`, and
> are added there. The `Recipe` below had fallen behind §2 of `09-recipes.md` and is corrected in
> place. Fields the schema carried and this catalogue did not name are now named here, each where
> its object is, so the catalogue stays the complete list it claims to be.

---

### 3.1 Tenancy

#### Organization

> The root of a tenancy tree. Owns nothing directly; contains projects and sub-organizations.

```
Organization
  id            OrgID          immutable, never reused
  name          string         mutable, unique within parent
  parent        OrgID?         null for a root organization
  quota         QuotaSet       what this org may consume (§5.1 of `05-scheduling.md`)
  status        active | suspended     reversible, from above only (§2.1 of 08-identity.md)
  delegation_ceiling  Duration?  the longest a grant beneath a set subject lasts in its subtree
                                 (§5.1 of 08-identity.md)
  placement_policy    PlacementPolicy?  how its subtree's workloads stand towards each other and
                                 how the nodes it owns are used (§2.2 of 05-scheduling.md)
  created       Timestamp
```

**Invariants**
- An organization's **reservations** can never exceed its parent's unallocated reservations. Its
  **limits** may exceed the parent's by the overcommit ratio the parent has enabled for each
  dimension (§5.1 of `05-scheduling.md`). Both are compared entry by entry of one shape, and held
  when a set is set, as "Setting a quota set" in §5.1 there says; a root organization holds no
  set.
- The tree has no cycles.
- Deleting an organization requires its subtree to be empty.

Recursive organizations are how reselling exists without a reseller feature (Rule K-14, Bet 4). A
hosting provider is a root org. Its customer is a child org with an allocated quota. That
customer's own customers are grandchildren. Nothing in the code distinguishes these levels.

#### Project

> A named grouping of workloads inside an organization, and the usual unit of delegation.

```
Project
  id            ProjectID      immutable
  org           OrgID
  name          string         mutable, unique within org
  quota         QuotaSet?      optional sub-limit within the org's quota
  labels        map[str]str
```

Most grants are scoped to a project. A community's "Minecraft network" and its "website" are two
projects; a moderator can hold authority over one and not the other.

> Added 2026-10-12: **label syntax**. Labels on projects, workloads and nodes use the syntax of
> Kubernetes labels, deliberately: a key is an optional DNS-subdomain prefix and `/`, then a name
> of at most 63 characters from `[A-Za-z0-9-_.]` beginning and ending alphanumeric; a value is
> empty or such a name. Operators already know it, and a selector reads the same in both systems.
> The API refuses anything else (`crates/korpis-core/src/labels.rs`).

#### Quota

> An enforced upper bound on resource consumption, inherited down the tenancy tree.

> Second external review, findings S16 and S78. Recorded in §17 of `23-walkthroughs.md`. The flat
> struct below was replaced by `QuotaSet` in §5.1 of `05-scheduling.md` (selector-scoped entries,
> each with reservations and limits), and this section did not say so; its invariants still forbade
> the overselling that §5.1 exists to allow. The struct is kept visible as the dimension list. The
> shape that holds it is `QuotaSet`.

```
Quota dimensions
  cpu           millicores
  memory        bytes
  disk          bytes
  disk_io       bytes/sec and ops/sec, as limits (§5.2 of `05-scheduling.md`)
  egress        bytes per calendar month of metering periods (UTC days)
  bandwidth     bits/sec
  workloads     count
  volumes       count
  endpoints     count
  dedicated_ipv4  count        exclusive, never overcommitted; IPv6 is not counted
  node_ports    count          ports of a node held by node_port and node_bound endpoints;
                               exclusive (§3 of 07-networking.md)
  devices_<class>  count       devices held, one each, a timeslice share counting one (§6.4 of
                               05-scheduling.md); added 2026-10-25
```

**Invariants**
- **Every dimension is enforced, by the component that can enforce it, and metered** (Rule K-3,
  P4). Per-workload resource dimensions are enforced by the kernel at the node, in the write path or
  the scheduler of the resource itself. Aggregate dimensions (an organization's total memory across
  fifty nodes) and count dimensions (how many workloads) cannot be enforced by any one kernel; they
  are enforced at admission, in the same transaction that would exceed them (§5.1 of
  `05-scheduling.md`). The first version said every dimension was enforced "by the kernel at the
  node", which no count could satisfy. A dimension that nothing can enforce is still not added.
- **`egress` is enforced by shaping, decided centrally and applied locally.** The control plane
  computes consumption against the period's budget from metering (§2 of `15-observability.md`);
  when a budget is exhausted it sets the workload's egress state in its assignment, and the agent
  applies the shaping the quota entry declares for that state (a floor rate, not a cut-off, unless
  the entry says otherwise). Who evaluates, where, and with what information: the control plane,
  from metered bytes; the kernel, from a rate.
  The budget's window is a calendar month of UTC days, summed as an invoice sums them (§2 of
  `15-observability.md`), and it begins again on the first: corrected 2026-10-21, when bandwidth
  was designed (§7 of `07-networking.md`). It said "bytes per metering period", written before a
  period was defined, and a period is now a UTC day, which would have made a monthly transfer
  allowance, what hosts sell, a daily one. What it costs: a host that bills from the day a customer
  signed up rather than from the first keeps that budget itself.
- Reservations sum against the parent and are never oversubscribed, counting what the parent runs
  itself beside what it gave (corrected 2026-10-22, Finding 139 of `23-walkthroughs.md`); limits
  may exceed the parent's by a declared ratio. Admission holds what it checks, at admission time,
  not at use time.

> Named `Quota`, deliberately not `Plan`, `Plan` is taken by §3.3 and the collision would be
> constant. A commercial "plan" is a billing concept and lives in the billing system (Rule K-12).

---

### 3.2 Identity and authority

#### Subject

> Anything that can act. Not necessarily a person, and not necessarily registered with Korpis.

```
Subject = User | Token | ExternalIdentity | Workload | Extension | Link | System | Anonymous
```

| Variant | Is | Example |
|---|---|---|
| `User` | a local account | someone who signed up |
| `Token` | a bearer credential, created holding the one grant it was issued for (§3.7 of `08-identity.md`) | a CI pipeline, a billing integration |
| `ExternalIdentity` | a claim from an identity provider | Discord user `123`, Discord role `@mod` in guild `456`, an OIDC group |
| `Workload` | a running workload acting for itself | a bot that restarts its own sibling |
| `Extension` | a running extension | a backup extension reading volumes |
| `Link` | whoever holds a share link, the bearer of the link-form grant it names (§6.1 of `08-identity.md`) | a moderator sent a link that restarts one server |
| `System` | the control plane acting on a decision already made | the reconciler correcting drift, the scheduler binding a placement, the bootstrap that creates the root grant, the authority renewing an admitted relay's certificate |
| `Anonymous` | whoever presented a credential that opened nothing, known only by where it came from | a script presenting a deleted token, a forged `Hello` |

> Added 2026-10-12, while writing the wire schema (`crates/korpis-proto`). Every `Effect` names an
> actor, and drift correction, placement, and installation bootstrap had none to name: they are
> authorized by an earlier approval or by installation itself (§9.1 of `08-identity.md`), not by a
> subject acting now. `System` holds no grant and can be issued none; it appears only as the actor
> of effects whose authority is the plan or approval they cite.

> Added 2026-10-12, while building the store (`crates/korpis-core/src/principal.rs`). Every subject
> has one **canonical key**: `user:<id>`, `token:<id>`, `workload:<id>`, `extension:<id>`,
> `system:<actor>`, `ext:<provider>:user:<id>`, or `ext:<provider>:set:<realm>:<member>`. Grants
> are indexed by it, effects record their actor as it, and it parses back to exactly one subject,
> which holds because every part but the last is refused if it contains `:`. The first schema
> stored the wire message instead; that tied an append-only audit log to the protocol's field
> names, and migration `0005_principal_keys.sql` records the correction.

> Added 2026-10-16, when failed attempts to authenticate began to be recorded (§10 of
> `10-api.md`). A failure has no subject to name, since the credential it presented opened
> nothing, and the effect recording it must name an actor: `Anonymous`, key `anonymous`, is that
> actor. Like `System` it holds no grant and can be issued none; the effect carries the source the
> attempt came from and what it named. `Link`, key `link:<grant>`, was added with share links on
> 2026-10-14 (§6.1 of `08-identity.md`) and was missing from this list and the one above until
> now.

`ExternalIdentity` being a first-class subject is what makes Bet 1 work. A grant can name *the
Discord role `@moderator` in guild X* directly. Nobody needs a Korpis account for that role to
carry authority, and losing the role removes the authority with no deprovisioning step.

#### Grant

> The only authority primitive. There are no roles.

```
Grant
  id            GrantID
  subject       Subject
  actions       []Action        e.g. workload.start, workload.console.write, backup.read
  scope         Scope           Organization | Project | Workload | Volume | Endpoint | ...
  conditions    Conditions
  parent        GrantID?        the grant this was attenuated from; null only for root
  delegation    DelegationPolicy   whether, how deep, and with which actions it may be attenuated
  issued_by     Subject
  via           {set, member}?  when issued by a member exercising a set grant (§5.1 of
                                08-identity.md)
  from_template TemplateRef?    provenance only; no authorization decision reads it
  issued_at     Timestamp
  revoked_at    Timestamp?
  label         string          what it is for, for people; no decision reads it (§3.7 of
                                08-identity.md)

Conditions
  expires_at    Timestamp?
  not_before    Timestamp?
  source_cidr   []CIDR?
  requires_mfa  bool
  requires_approval_by  Scope?   // this grant can propose, not apply (it may read), see §3.3
  max_uses      int?             // redeemed online, see §6.2 of 08-identity.md
```

**Invariants**
- **A grant can never exceed its parent.** Its actions are a subset, its scope is contained, its
  conditions are at least as restrictive, and its delegation policy is no wider. This is checked at
  issue time and re-checked at use time, because a parent may have been revoked in between.
- **Only a grant whose delegation policy allows it can have children** (§3.2 of
  `08-identity.md`). A link-form grant never can.
- Revoking a grant revokes its entire subtree, transitively and immediately.
- Every authority in the system traces to a root grant through an unbroken chain. There is no
  ambient authority anywhere, including for administrators.
- "Administrator" is a grant with a wide scope and no expiry. It is not a flag, a boolean column,
  or a branch in the code (P6).

**Lifecycle**

```
issued ──use──▶ issued
   │
   ├──expires_at reached──▶ expired
   ├──max_uses reached────▶ exhausted
   ├──revoked──────────────▶ revoked  (cascades to subtree)
   └──parent revoked───────▶ revoked
```

The operation RBAC cannot express, which this makes trivial:

> A moderator holds `workload.restart` on project *Survival*, expiring in 90 days, with a delegation
> policy that permits one further level. They attenuate it into a link: `workload.restart` on
> **one** workload, expiring in 24 hours, `max_uses: 3`, no account required. They send it in a
> Discord DM. It cannot outlive their own grant, revoking theirs kills it, and the link itself can
> delegate nothing.

#### GrantTemplate

> A named, reusable bundle that expands into grants. A role in the user interface, not in the
> model.

```
GrantTemplate
  id            TemplateID
  organization  OrgID           the owner; offered to its whole subtree
  name          string          "Moderator", "Billing integration", "Read-only auditor"
  version       int             incremented on every edit; each version kept
  actions       []Action
  scope_pattern ScopePattern    the kind it is applied to, and optionally a label selector below
  conditions    TemplateConditions   relative defaults, overridable at issue time
  delegation    DelegationPolicy     required
  retired_at    Timestamp?
```

§4 of `08-identity.md` is the one definition and this is a copy of it, kept so the catalogue is
complete.

The `Grant`-only model in §3.2 is correct and it is unusable on its own. Nobody wants to assemble
an action list by hand every time someone joins a moderation team, and the first thing every
operator will ask for is "give this person the same access as that person."

A template answers that without reintroducing roles. Applying a template **issues ordinary grants**
and then disappears from the picture:

- The issued grants are normal grants. They attenuate, expire, revoke, and audit like any other.
- They carry the template and version they came from as provenance, which authorization never
  reads. Editing a template later does not silently widen authority that has already been handed
  out, a property RBAC does not have, and the source of a whole class of production accidents.
  Corrected 2026-10-19: this said they carry no back-reference, which finding S34 of the second
  external review had already replaced in §3.5 and §4 of `08-identity.md`; the catalogue was left
  saying the opposite.
- Authorization code never sees a template. It sees grants. There is exactly one authorization path
  (P6).

Templates are convenience at the edge. The model stays pure; the interface stays usable.

#### Identity provider binding

```
IdentityBinding
  provider            ProviderRef     the identity provider extension that verifies it
  selector            guild+role · guild+user · oidc claim+value · saml attribute+value
  subject             ExternalIdentity
  kind                individual | set
  created_by          operator | first_use
  template            TemplateRef?    for set bindings
  constraints         BindingConstraints
  reverify_interval   Duration
  allows_irreversible bool            for set bindings
  state               verified | unverified
  last_verified_at    Timestamp
```

> Second external review, finding S34. This struct had four fields and `08-identity.md` §5 had a
> different set, extended again in §5.1. §5 of `08-identity.md` is now the one definition and this
> is a copy of it, kept so the catalogue is complete.

---

### 3.3 The workload and its state

This is the centre of the model.

#### Workload

> The identity of a thing that runs. Deliberately almost empty: it is a stable name to which state
> attaches.

```
Workload
  id            WorkloadID     immutable, never reused
  project       ProjectID
  organization  OrgID          the project's, denormalized so authorization reads one row
  name          string         mutable, unique within project
  labels        map[str]str
  intent        IntentID       the newest declared intent, the head of the chain
  target        IntentID?      the approved intent the system converges toward; null until the
                               first plan is approved
  power         Power          the newest power declaration (below): the power head
  power_target  PowerVersion?  the approved power declaration nodes are told; moved only by
                               approval, null until one is approved
  observation   ObservationID? the most recent observed state, null if never observed
  set           SetID?         the declared set it was created through or adopted into (below)
  classification []Override    the operator's layer of the classification chain (below), at most
                               one per action
  version       int            its metadata's version, the classification among it: what a
                               change to them compares against
  unreproducible {at, reason}? set the first time what it runs stopped following from its
                               recipe and lockfile alone (§4 and §9 of `09-recipes.md`);
                               never cleared
  created       Timestamp
  deleted_at    Timestamp?     soft; the id is retained forever
```

**Invariants**
- `id` is never reused, even after deletion. Logs, backups, grants, and metering records outlive
  the workload and must never be reattributed to a different one.
- `name` is a label for humans and may be changed or reused. Nothing references a workload by name.
- **`intent` and `target` are different pointers, and only `target` reaches a node.** Declaring an
  intent moves `intent`. Approving the plan for it moves `target`, and only then does the change
  enter the workload's assignment (§4.8 of `02-architecture.md`). While they differ, the workload is
  awaiting approval or scheduled, and nothing on any node has changed.
- **`unreproducible` is set once and never cleared.** A workload whose install took a step
  provider's unpinned answer (§4 of `09-recipes.md`), or that was declared from an `unverified`
  import (§9 of the same), runs bytes that its recipe and lockfile do not determine, and no later
  install undoes that: the files are in its volumes. The control plane sets it when it resolves
  such a step, with what the step resolved to, or when it accepts such a declaration, and the
  first reason stands. It is a label, not a gate: it says which workloads the promise of §2 of
  `09-recipes.md`, that one digest runs the same bytes, does not cover.

> Second external review, finding S3. Recorded in §17 of `23-walkthroughs.md`. The first version had
> one pointer, `intent`, which the compare-and-swap of §3.2 of `03-state.md` moved at declaration,
> before any plan was approved. The protocol published "the current intent", so a node could
> converge toward a change nobody had approved, which bypassed P5 and `requires_approval_by`, and a
> scheduled change ("approve now, apply in a window") had nowhere to wait.

> Added 2026-10-13, from the implementation. Power had the same defect: one pointer, moved at
> declaration, and the assignment carried "the workload's power". A stop that waits for
> confirmation because the recipe declares stopping disruptive would have reached the node the
> moment it was declared. Power now has a head and a target too, and only approving a power plan
> moves `power_target`. The first approval of a workload's first intent approves its first power
> declaration with it, because that plan's steps (`create`, then `start`) were computed with it.

#### Intent

> What someone declared should be true. Immutable and versioned.

```
Intent
  id            IntentID       immutable
  workload      WorkloadID
  version       int            monotonic per workload
  parent        IntentID?      the intent this was derived from
  declared_by   Subject
  declared_at   Timestamp
  managed_by    SetID?         the declared set whose apply declared it; null for any other
                               declaration (the panel, a chat command, `korpis workload ...`)

  // --- the declaration itself ---
  recipe        RecipeRef      name + version + digest (see 09-recipes.md)
  lifecycle     persistent | task | scheduled
  schedule      CronExpr?      required iff lifecycle = scheduled
  timezone      string?        the IANA zone `schedule` is read in; UTC when unset; only with
                               a schedule (§4.1 of 05-scheduling.md); added 2026-10-18
  runs          RunPolicy?     concurrency, max_active, retries, backoff, timeout, retention,
                               catch_up; only for task and scheduled, defaults when unset (§4
                               of 05-scheduling.md); added 2026-10-18
  console       none | logs | interactive
  runtime       RuntimeSpec    driver + isolation tier + driver-specific config
  resources     ResourceSpec   cpu, memory, disk, io, bandwidth (§5.2 of 05-scheduling.md)
  devices       []DeviceRequest  class, count, selector, mode: passthrough | timeslice |
                               mediated, passthrough when unset (§6.3 of 05-scheduling.md);
                               added 2026-10-25
  volumes       []VolumeSpec
  endpoints     []EndpointSpec
  health        HealthSpec
  config        map[str]Value  validated against the recipe's schema (Rule K-17)
  placement     PlacementSpec  constraints, affinities, preferred region
  continuity    ContinuitySpec on_lease_expiry: derived | keep_running | stop
                               (§4.5 of 02-architecture.md); derived when unset
  dependencies  []Dependency   name, target (workload, endpoint, or selector) in the same
                               project, inject, required (§5 of 07-networking.md)
  streams       StreamSpec     its console stream's rate, burst, and retention (bytes or a
                               duration), and how long its offloaded segments are kept; each
                               left unset is the default (§4 and §5 of 14-streams.md)
  egress        EgressPolicy   where it may connect out beyond the egress default, and its
                               own limits, which only lower the default's (§6 of
                               07-networking.md); added 2026-10-15, moved from each endpoint
```

> Corrected 2026-10-14, when streams were built. §5 of `14-streams.md` made a stream's rate "a
> declared field with a sane default", and the intent had no field to declare it in, so the one
> limit that keeps a workload's logging from filling its node could not be set. Rule E of
> `20-roadmap.md` meant to freeze the schema in Phase 0; this field was missed there, and adding
> it is recorded rather than passed over. An intent stored before it reads as one with the
> defaults.

> Corrected 2026-10-18, when runs were designed for building (Findings 98 and 100 of
> `23-walkthroughs.md`). §4 of `05-scheduling.md` defined `RunPolicy` and the intent had no field
> to declare it in, and `schedule` was a cron expression in no stated time zone, so a nightly job
> meant for 03:00 local time ran at an hour that moved with the seasons. Both fields are added
> under rule E of `20-roadmap.md` as `streams` and `egress` were; an intent stored before them
> reads as one with the default policy, in UTC.

> Corrected 2026-10-15, when a workload's own network policy was built (Finding 55 of
> `23-walkthroughs.md`). Outbound rules were declared on each endpoint, where no node could
> evaluate them, since a connection a workload opens leaves from no endpoint (§6 of
> `07-networking.md`). They are the workload's, so `egress` is added here; the endpoint's field is
> kept, because rule E of `20-roadmap.md` lets a later phase implement a frozen schema and not
> change it, and a declaration that sets it is refused with where the rules now go. An intent
> stored before it has no outbound rules of its own and reads as one with none.

**Invariants**
- An `Intent` is **never mutated**. Changing anything creates a new `Intent` with `version + 1` and
  `parent` set to the previous one.
- `config` is validated against the recipe's declared schema at declaration time. Invalid intents
  are rejected before they exist, not discovered at apply time.
- Every intent records who declared it. There is no anonymous change.

Because intents are immutable and chained, the full history of a workload's declared state is a
linked list, and rollback is "declare intent N again" rather than an inverse operation (P9).

#### DeclaredSet

> A named group of workloads that one declarative source owns: the files a pipeline applies with
> `korpis apply -f servers/ --set community-servers` (§5 and §6 of `13-surface-cli.md`).

> Added 2026-10-13, from the implementation. §6 of `13-surface-cli.md` said that "every `Intent`
> carries `managed_by`" and that drift behaviour "is declared per set", and neither the field nor
> the set existed anywhere in the model, so nothing could hold a set's mode or tell an
> out-of-band change from the set's own.

```
DeclaredSet
  id            SetID          immutable
  project       ProjectID      a set and its members live in one project
  name          string         unique within the project
  mode          advisory | strict    what declaring through it does with drift (§6 of
                               13-surface-cli.md)
  version       int            compare-and-swap on mode changes
  created_by    Subject
  created_at    Timestamp
```

**Invariants**
- **A workload belongs to at most one set, and a set touches only its own.** A workload joins a
  set by being created through it, or by a declaration through it that asks to adopt a workload
  belonging to no set. A declaration through a set onto another set's member is refused, and so
  is one onto a workload in no set that did not ask to adopt it: a hand-made workload is never
  claimed, and so never pruned, by someone's pipeline.
- **Drift is derived, never stored.** A member has drifted when its head intent's `managed_by` is
  not its set: someone declared it through the panel, a chat command, or an imperative command.
  The head already names who did it and when, so recording drift would be a second copy of a fact
  that could disagree with the first.
- **Declaring through a set onto a drifted member** is decided by the control plane at
  declaration, which holds the workload's set, the head, and the declaration's completed body:
  - a body equal to the head's is the files catching up with the fix, and is accepted in either
    mode, recorded as `set.drift_adopted`;
  - otherwise a `strict` set declares over it, and an `advisory` set refuses with `CONFLICT`
    reason `set.drifted` unless the declaration asks to `reconcile`; either way the discarded
    intent and its author are recorded as `set.drift_discarded`.
- A set is created by the first declaration through it, in `advisory` mode, with the authority
  that declaration already needed; nothing else creates one. Changing a set's mode is project
  configuration and needs `project.write`. Reading a project's sets needs `project.read`.

The cost is one more refusal reason an operator has to understand, and one more pointer on two
objects. A set spanning projects is not expressible: a pipeline that applies to three projects
holds three sets of the same name, because the project is where workload authority and names
live, and a set wider than that would need authority over objects its applier cannot touch.

#### Power is declared separately from configuration

> External review, finding R7. Recorded in §16 of `23-walkthroughs.md`.

An earlier version of this schema carried `state: running | stopped` inside the intent body. It is
the obvious place for it and it is the wrong place, for a reason that only shows up under this
market's actual usage: **start and stop are the most frequent operations there are**, and every one
of them would copy the entire body into a new immutable row.

Two things break. The intent chain stops being a configuration history and becomes a power log, so
"roll back to intent 41" is no longer a meaningful configuration target when the only difference
between 41 and 42 may be that one is stopped. And the retention problem of open question 2 in
`03-state.md`, where an autoscaler produces millions of intents, gets worse on its own.

```
Power
  workload      WorkloadID
  desired       running | stopped
  cycle         int            incremented to request one restart; performed once per value
  version       int            monotonic, its own sequence
  set_by        Subject
  set_at        Timestamp
```

Power is versioned, attributed, and reconciled exactly like everything else, so nothing about P2 or
P5 changes: stopping a workload still produces a `Plan` and an `Effect`, and still names the grant
that authorized it. What changes is that it does not produce an `Intent`. It reaches the node in
the workload's assignment (§4.8 of `02-architecture.md`), and a restart is a higher `cycle`, which
the agent performs once and records (§4.1 of `02-architecture.md`).

The test of the split is P9's promise. Re-declaring intent 41 now restores a configuration and says
nothing about whether the workload should be running, which is what a rollback should mean.
Anything else conflates "what this workload is" with "whether it is on right now".

#### Observation

> What the node agent actually sees. Never merged with `Intent`.

```
Observation
  id            ObservationID
  workload      WorkloadID
  node          NodeID
  observed_at   Timestamp
  assignment_seen  int         the assignment generation the agent has applied
  intent_seen   IntentID       the target intent the agent has converged to
  power_seen    {version, cycle}  the power declaration and restart cycle it has performed
  plan_progress {plan, step}?  the last plan step it completed, while one is assigned

  state         running | stopped | starting | stopping | crashed | unsatisfiable | unknown
  reason        string?        for unsatisfiable: the structured reason (§7 of 05-scheduling.md)
  health        healthy | unhealthy | unknown
  exit_code     int?
  runtime_id    string         container/VM id on the node
  resources     ResourceUsage
  endpoints     []EndpointStatus
  volumes       []VolumeStatus
  since         Timestamp      when the state last changed
```

**Invariants**
- `Observation` is written only by the node agent. `Intent` is written only by the control plane.
  They are separate objects with separate writers and separate trust levels, merging them into one
  spec/status record invites exactly the "the panel says stopped but it is running" class of bug
  (P1).
- When the agent is unreachable, `state` is `unknown`. Korpis never reports a remembered value as a
  current one (P4).
- The `*_seen` fields make convergence lag visible: if any trails the assignment, the workload has
  not caught up yet, and the UI can say so precisely (§4.3 of `02-architecture.md`).
- **An observation reports only what the node can know.** Whether a plan has failed, been
  cancelled, or is waiting for approval is a fact about the plan, held by the control plane, and
  never appears as an observed state.

#### Plan

> The computed difference between a declaration and reality, persisted, inspectable, approvable.

```
Plan
  id            PlanID
  workload      WorkloadID
  kind          intent | power | retire | migrate
                | delete_volume | snapshot_volume | rollback_volume | delete_snapshot
  operation     OperationID?   the parent, when generated by an Operation (§3 of 05-scheduling.md)
  from_intent   IntentID?      Workload.target when computed
  from_power    PowerVersion?  Workload.power_target when computed
  to_intent     IntentID?      required iff kind = intent
  to_power      PowerVersion?  required iff kind = power
  to_volume     VolumeID?      required iff the plan operates on a volume (the last four kinds);
                               the plan's workload is the one holding it, or the one that created
                               it, retired or not, when none does (§7 of 06-storage.md)
  computed_at   Timestamp
  computed_by   Subject
  proposed_under GrantID?      the grant that permitted the declaration; confirmation
                               re-evaluates it, and requires_approval_by is read from it; absent
                               only on a plan a restore recovered whose grant the restored store
                               does not hold (§8 of 03-state.md)

  steps         []Step
  impact        Impact         disruptive?, downtime estimate, data at risk, cost delta, the
                               exposure of each endpoint it creates or changes (§3 of
                               07-networking.md), and why it does or does not auto-approve
  status        proposed | approved | queued | applying | applied
                | failed | cancelled | rejected | expired | superseded
  completed     int?           the last step whose completion effect has been ingested
  apply_after   Timestamp?     approved now, applied in a window
  expires_at    Timestamp
  approved_by   Subject?
  approved_at   Timestamp?
  approval      auto | confirmation | authority   what it needs before it applies (below)
  approval_scope Scope?        for authority: where the approver's grant must reach

Step
  index         int
  action        create_volume | pull_recipe | stop | migrate | reconfigure_network | ...
  executor      control_plane | node | extension    (§4.9 of 02-architecture.md)
  target        object reference
  reversible    bool
  timeout       Duration       how long the step may run before the plan reports it stuck
                               (§4.3 of 02-architecture.md)
  disruptive    bool           resolved through the override chain below, never inferred
  classified_by vocabulary | driver | recipe | operator   the layer that decided `disruptive`
  reason        string         why, in words
  detail        map[str]Value
```

> Second external review, findings S1, S2, S4, and S16. A plan with no intent change (a stop, a
> deletion, a migration of an unchanged workload) had no valid `to_intent`, because the field was
> required, and its steps had no unique key, because the key was built from the intent version.
> `kind` and an optional `to_intent` give those plans a shape, and steps are keyed by
> `(plan.id, index)` (§4.7 of `02-architecture.md`). The status list now includes the two states
> §7 of `03-state.md` already used and the model did not name: `queued` and `cancelled`.

**Invariants**
- Every change to a workload's **declared** state passes through a `Plan`: a new intent, a power
  change, a retirement, a migration. There is no code path that changes what a node is assigned
  without one (P5).
- A plan is computed against a specific `from_intent`. If reality moves underneath it, the plan is
  marked `superseded` and must be recomputed, a plan can never be applied against a state it was
  not computed for.
- Only a plan for the head intent is approvable. Declaring a newer intent supersedes every
  unapproved plan for an older one.
- **A failed or cancelled plan is resumable from `completed`.** Its completed steps' resources are
  retained and named (§3.1 of `05-scheduling.md`), and the next plan for the workload is computed
  against that state rather than against an assumption about it.
- **An intent plan's steps are computed from the last intent the workload reached.** That is the
  target, unless the plan approved to reach it failed; then it is the intent that plan started
  from, followed back through any failed plan before it, or none. Cancelling already moves the
  target back. The plan's `from_intent` still names the target, which is what approving it
  compares. Every step is idempotent, so a step the failed plan completed is completed again as a
  no-op, and an install with the same actions resumes at the action that failed (§3.1 of
  `05-scheduling.md`): declaring the same body again is how a person resumes a failed plan.

> Added 2026-10-15, Finding 51 of `23-walkthroughs.md`. Plans were computed from the target, which
> approval moves before anything runs. A create plan that failed at its install left the target at
> an intent nothing had reached, so the next plan for that same intent had no steps: `create` and
> `start` were never planned again, and the install's saved progress was never used.
- **A node holds only what a plan made or is making.** It is told the intent and power the
  workload's plans put on it, or that the plan it may act on now is reaching, not the target (§4.8
  of `02-architecture.md`): the last intent reached, as above, passing also any plan that has not
  begun. So `apply_after` holds: a plan approved for a later window, or queued behind another,
  changes nothing on the node until it begins. A workload no plan has brought to any intent is told
  its target with nothing to run, so the node keeps what the plan's steps saved and starts nothing.

> Added 2026-10-15, Finding 52 of `23-walkthroughs.md`. Nodes were told the target, which approval
> moves before anything runs, and a node runs what it is told: a create approved for a later
> window started at once without its pull, volume, or install, a create whose plan failed started
> without its install, and a change waiting for its window was applied by the next crash restart.
- Plans expire. A stale plan is not an approvable plan.
- **A retire plan moves no pointer, and approving it freezes the workload.** Deleting a workload
  proposes one: stop, detach and retain every volume, remove the runtime (§4.8 of
  `02-architecture.md`), and then delete, each by its own irreversible step, only the volumes the
  deletion named (§7 of `06-storage.md`). Removing a runtime is disruptive by the vocabulary's
  floor, so it always waits for a human. Once it is approved, every other proposal is superseded
  and nothing else may be declared or approved until it finishes or is cancelled. `deleted_at` is
  set when the retirement is complete, which releases the name and the quota: when the node reports
  the runtime removed, or at approval for a workload no node holds, whether nothing was ever
  approved for it or no node could take what was, whose plan the control plane executes alone;
  approval checks again that no node took it meanwhile (Finding 61 of `23-walkthroughs.md`).
- A plan containing a step marked `reversible: false`, deleting a volume, releasing an IP, always
  requires explicit approval. A plan whose steps are all non-disruptive is auto-approved by default
  policy, and **`disruptive` is declared, never inferred** (below).

> Added 2026-10-12, from the implementation (`crates/korpis-core/src/workload/plan/approval.rs`).
> "Explicit approval" named two different requirements, and §3.6 of `08-identity.md` gives only one
> of them its rules. Read literally, a lone operator could never stop a game server whose recipe
> declares stopping disruptive, because nobody may approve their own plan, while §4 of
> `13-surface-cli.md` has the same operator apply a plan they reviewed with `--plan`. Every plan
> now records which one it needs:
>
> - **Auto**: every step reversible and non-disruptive and the impact known; applied at once.
>   The plan records its proposer as `approved_by`, under the grant that permitted the
>   declaration, and a `plan.approve` effect says it was approved by default policy: no other
>   subject's authority is claimed for it.
> - **Confirmation**: the proposer's grant permits the action outright, but the plan is not
>   auto-approvable, or its proposer asked to confirm it (`propose`, §4 of `13-surface-cli.md`).
>   It waits in `proposed` until someone confirms it, and the proposer may: P5
>   asks that a human see the change, and the proposer reading the plan is that human. Confirming
>   re-evaluates the proposer's authority for the plan's action.
> - **Authority**: the plan was proposed under a grant with `requires_approval_by`. It needs a
>   different subject holding `plan.approve` over the approval scope, with the chain rule of §3.6
>   of `08-identity.md`; the proposer can never supply it, however the plan is classified.

**Where `disruptive` comes from**

> External review, finding R9. Recorded in §16 of `23-walkthroughs.md`.

"Auto-approved if non-disruptive" is a policy engine hidden inside an adjective, and the adjective
is not a property of the step. `stop` is disruptive to a game server with forty players on it and
routine for a stateless web service behind a load balancer. If nothing says where the answer comes
from, auto-approval becomes a quiet way around P5's human-approval promise, decided by whoever
implemented the classifier.

So the classification has one source and one override chain, both in the Plan and both visible:

| Layer | Declares | Wins over |
|---|---|---|
| the action vocabulary | a floor: `delete_volume` is disruptive and irreversible, always | nothing, this cannot be relaxed |
| the driver | how a tier changes it: `stop` on a `vm` costs a boot, on a `process` it does not | the floor, upward only |
| the recipe | what this software costs to interrupt (`disruption` in §5 of `09-recipes.md`) | the driver, upward only |
| the operator, per workload | a deliberate override, recorded | everything, and it is an `Effect` |

Every layer may only make a step **more** disruptive than the layer beneath it, never less. That
asymmetry is the whole safety property: the vocabulary's floor cannot be argued away by a recipe
author or a driver, and an operator who wants a quieter approval flow has to say so on the record.

> Clarified 2026-10-12, from the implementation (`crates/korpis-core/src/workload/plan/step.rs`).
> "Everything" in the operator's row means everything above the floor: an operator's override may
> make a step less disruptive than the driver or the recipe said, never less than the vocabulary's
> floor, which the first row says cannot be relaxed. Each step records the layer that decided its
> classification and the reason in words, so "both in the Plan and both visible" holds for the
> answer as well as for the rules.

> Added 2026-10-13, from the implementation. The operator's row had no way to be set. It is now
> `Workload.classification`, replaced as a whole by `SetClassification` under the workload's
> version, with the action `workload.classify` (§5 of `10-api.md`) and a required reason; the
> effect records the set before and after. An entry that would relax the floor is refused rather
> than stored and ignored, so the record says only what takes effect. Plans computed after a
> change read the new set; a plan already proposed keeps the classification its proposer saw,
> because its approval kind is what they were shown.

> Added 2026-10-13, from the implementation. Who evaluates the driver's row, and with what: the
> control plane, when it computes a plan, from the `disruption` each node's drivers declared
> (§4.1 of `04-runtimes.md`), as the union over every node that serves the workload's
> organization and offers its tier. Until then every plan used the floor for that row.

`Impact.downtime_estimate` and `Impact.data_at_risk` follow the same rule. Where they cannot be
computed they are reported as `unknown` rather than zero, and a plan whose impact is `unknown` is
**not** auto-approvable. P4 applied to the one number that decides whether a human sees the change.

> Added 2026-10-19, when the recipe's `pre_stop` hook was built (§3 of `09-recipes.md`). The
> downtime of a stop is still `unknown`, since how long the software takes to come back is
> measured nowhere, but its upper half is known, and it is shown. Every step that stops a
> running object (`stop`, `restart`, `retire`) carries `stops_within_seconds` in its detail: the
> hook's `wait_for_exit` and then the node's grace of thirty seconds, which is the longest the
> node lets the stop take before it kills the process (§6 of `02-architecture.md`). What
> evaluates it: the control plane, when it computes the plan, from the recipe it already reads to
> classify the step and the grace, which is a constant of the platform and the node's default
> alike. The CLI prints it with the step's other detail, and the panel says it beside the step.
> What it does not say: an agent configured with another grace (only tests do) stops within its
> own, not the one the plan shows.

**Plans govern intent changes, not drift correction**

There is a tension here that has to be resolved explicitly, because getting it wrong breaks either
P5 or the system's viability.

The reconciler runs continuously. A crashed process gets restarted; a container that vanished gets
recreated; a firewall rule that drifted gets rewritten. If each of those produced a `Plan`, a node
running two hundred workloads would generate a constant stream of plan objects that nobody reads,
and the audit trail would be noise. If none of them produced anything, there would be two mutation
paths and P5 would be decorative.

The resolution is that these are different operations:

| | Produces a Plan | Authorized by |
|---|---|---|
| **Intent change**: a subject declares something new | yes, always | the subject's grant, at declaration |
| **Drift correction**: reality diverged from an intent already approved | no | the approval of that intent |

Converging toward an approved intent is not a new decision. It is the execution of a decision
already made and already audited. It still emits `Effect`s (every restart, recreate, and repair is
recorded) but it does not require a new plan, because nobody is choosing anything.

This is also what keeps the interactive path fast. Pressing Start declares a new `Power` version
with `desired: running`, which is a declaration, so it produces a plan of kind `power`. That plan's
steps are non-disruptive, so default policy auto-approves and applies it in the same call. The user
experiences one click; the system still has a complete declaration → Plan → Effect record, and the
intent chain is untouched, which is what finding R7 was for. The plan object is not a round trip,
it is a receipt.

> Second external review, finding S2. This paragraph said that pressing Start changes
> `state: stopped → running`, "which is an intent change", fifty lines after R7 had moved power out
> of the intent precisely so that it would not be one.

This single object is Bet 3, and it produces as consequences: dry-run (compute the plan, do not
apply it), approval workflow (`requires_approval_by` on a grant), scheduled change (approve now,
apply in a window), rollback (plan toward an older intent), and honest audit (the plan and its
effects are the record).

It is also what makes a chat surface safe to grant authority to. A Discord bot posts the plan with
its impact assessment, someone with the right grant presses Approve, and the audit trail is
complete, without anyone holding standing authority to mutate production from a chat message.

#### Effect

> What actually happened. Append-only.

```
Effect
  id            EffectID
  plan          PlanID?        null for events not originating from a plan
  step          int?
  workload      WorkloadID?
  node          NodeID?
  object        ObjectRef?     what it is about, the object its action was authorized over
                               (§4 of 15-observability.md)
  organization  OrganizationID?  the organization whose subtree holds the object, resolved
                               when it is written; none for a node or for nothing
  actor         Subject
  relay         Subject?       the client that relayed what the actor presented (§5.2 of
                               08-identity.md)
  grant         GrantID?       the grant that authorized it
  at            Timestamp      when the control plane ingested it, the only ordering clock
  occurred_at   Timestamp?     the originating node's reading, informational (§4 of 03-state.md)
  origin        {node, seq}?   for node-originated effects, the journal position (§4.10 of
                               02-architecture.md); the pair is unique
  action        string
  outcome       succeeded | failed | skipped | rolled_back | denied
  error         Error?
  before        map[str]Value?
  after         map[str]Value?
  count         int            repeats coalesced into this one (§4 of 15-observability.md); 1
                               for an effect that stands alone
  window_end    Timestamp?     the last repeat it covers
```

**Invariants**
- Append-only. Never updated, never deleted. Retention is by archival, not mutation.
- Every effect names the **grant** that authorized it, not merely the actor. "Who did this" is
  answerable; so is "under what authority, delegated from whom". An effect the control plane
  writes as its own actor (a placement, a lease, a step a node executed) names the grant of the
  decision it carries out: the plan that approved the target it acts on, or with none the plan
  most recently approved. A denial names no grant, because none authorized it; it names its
  subject and the action that was missing (§4 of `15-observability.md`).

> Clarified 2026-10-13, from `scripts/e2e-phase0.sh`. The invariant read as if a denial could
> name a grant, and the scheduler's bind and the lease issued for it named none, though both act
> on an approval; §4 of `20-roadmap.md` records the correction to the done-when that said so.
- An effect of a control-plane decision commits in the same transaction as the decision. An effect
  of something that happened on a node is ingested exactly once, in its journal order, in the same
  transaction as the state it explains (§4.10 of `02-architecture.md`).

`denied` is the outcome of a refused action, which §4 of `15-observability.md` records as an effect
like any other; the outcome list had no value for it until the wire schema was written (2026-10-12).

Audit is not a separate subsystem writing a parallel log that can drift from reality. The effect
stream *is* the record of what happened.

---

### 3.4 Placement

#### Node

> A machine running the Korpis agent.

```
Node
  id            NodeID         immutable
  name          string
  organization  OrgID          the organization it was enrolled into; immutable
  labels        map[str]str    region, rack, arch, os, gpu, storage-class, ...
  taints        []Taint        workloads must tolerate these to land here
  capacity      Resources      total
  allocatable   Resources      capacity minus system reserve (§2.1 of 05-scheduling.md)
  allocated     Resources      sum of what its placements reserve
  drivers       []DriverInfo   which runtime drivers this node offers, with capabilities
  devices       []Device       the devices its agent found described, below; added 2026-10-25
  cores         []Core         the physical cores it offers for dedication, below; added
                               2026-10-25
  storage_classes []StorageClassInfo  the classes its helper provides, with what each can do
                               (§2 of 06-storage.md)
  reachability  public | edge_only    whether inbound traffic can reach it directly (§9 of
                               07-networking.md)
  admin         active | cordoned | draining | decommissioned      what the operator decided
  connectivity  registering | connected | unreachable | unknown    whether it is heard
  overlay       available | unavailable    measured (§9 of 07-networking.md): available when it
                               reaches every member of the mesh it is paired with
  overlay_key   bytes          its WireGuard public key, as its agent reports it; the private key
                               never leaves the node (§9.1 of 07-networking.md); designed
                               2026-10-25
  overlay_port  uint32         the UDP port its mesh listens on, as its agent reports it
  pools         []Prefix       its workloads' address pools as it reports them; the mesh routes
                               only the one given at enrollment, or the old default, and the
                               prefix its identity derives (§9.1 of 07-networking.md)
  peers         []PeerReach    each member it is paired with that the caller may read, and
                               whether the pair is reachable: both nodes report a handshake with
                               the other younger than 180 seconds; what the scheduler asks of a
                               dependency across nodes; `overlay` to `peers` built 2026-10-25
  overlay_outside string       why it is outside the mesh, where it reports a side and is
  ingress       bool           whether its agent holds ports 80 and 443 for its ingress proxy;
                               required of an ingress endpoint's node without an edge tier
                               (§3.2 of 07-networking.md)
  status        derived from the two, below
  agent_version string
  last_seen     Timestamp
```

**A node belongs to the organization it was enrolled into.** Enrollment is `node.enroll` scoped
to one organization (§7 of `02-architecture.md`), and the node stays there: it is contained in
that organization for every grant (§3.3 of `08-identity.md`), and it serves that organization and
every organization below it, never a sibling or an ancestor (§2.1 of `05-scheduling.md`). A host's
fleet is enrolled into its root organization and so serves every customer; a customer who enrolls
a machine of their own gets a node only their own subtree lands on.

**`status` is derived, from two states that change for different reasons.** `admin` changes when
the operator decides; `connectivity` changes when the network does. The status a client shows is
`decommissioned` if the node is; otherwise `registering`, `unreachable`, or `unknown` while it is
not heard, because that is what needs attention; otherwise `draining`, `cordoned`, or `ready` from
the operator's decision. Only `ready` (active and connected) takes new placements.

> Correction, 2026-10-13, from the implementation. Nodes had no owner, so no grant but one naming
> the node could cover it (§3.3 of `08-identity.md`). And `status` was one field holding both
> states, so a cordoned node that dropped off the network and came back returned as `ready`: the
> lifecycle below said `unreachable ──▶ ready`, and a network blip undid the operator's cordon.
> The cost of two states is one more field a client renders; the derived `status` keeps the single
> word a list shows.

**There is no `Location` object.** Pterodactyl has Nodes inside Locations; Pelican replaced its
nest/location hierarchy with tags and was right to. Region, rack, datacentre, and provider are
**labels**, and placement uses label selectors. A rigid two-level hierarchy cannot express "same
rack", "different power feed", "has NVMe", or "EU only", a label selector expresses all of them.

`drivers` carries **declared capabilities**, borrowed from Nomad's task driver interface: whether a
driver can send signals, exec into a running workload, attach an interactive console, hot-resize,
snapshot, or live-migrate. The scheduler asks; it never assumes (Rule K-9).

```
Device
  id            string         its Container Device Interface name, vendor.com/kind=name;
                               unique on its node
  class         gpu | usb | pci | tpm | serial
  attributes    map[str]str    its CDI annotations, which a request's selector matches
  host_nodes    []string       the host device nodes its own edits give; a device whose nodes
                               contain another's is the same hardware (§6.1 of
                               05-scheduling.md)
  shares        int            how many workloads may hold it at once in `timeslice`; 0 offers
                               `passthrough` alone
```

**A device is a fact of its node, and what holds it is a fact of placements.** The agent reports
the devices the Container Device Interface's files describe on its machine (§6.1 of
`05-scheduling.md`), as it reports its capacity, and a device carries no record of who holds it:
`Placement.devices` does, so a node's held devices are derived as its `allocated` is. Added
2026-10-25, when devices were designed.

```
Core
  id            int            the lowest logical CPU of the core; unique on its node
  cpus          []int          its logical CPUs, the siblings that share its execution units
  numa          int            the NUMA node its CPUs belong to
```

**A core is offered whole, and what holds it is a fact of placements.** The agent reports the
cores the kernel's topology describes, all but the one holding CPU 0 unless the operator names
them (§5.2 of `05-scheduling.md`), and `Placement.cores` records which a placement holds, so a
node's held cores are derived as its devices are. Added 2026-10-25, when dedicated cores were
designed.

**Lifecycle**

```
admin          active ⇄ cordoned ──▶ draining ──▶ (empty) ──▶ decommissioned
               cordoned and empty ──────────────────────▶ decommissioned
               any, removed as compromised ─────────────▶ decommissioned
connectivity   registering ──▶ connected ⇄ unreachable
                                   ▲
               unknown ────────────┘  (after a control-plane restart, until heard)
```

Reconnecting returns a node to the `admin` state it had: a cordoned node comes back cordoned. A
decommissioned node does not come back: its key opens no session, and removing it ends the one it
held (§3 of `18-operations.md`).

A node is `connected` from a verified Hello (§7 of `02-architecture.md`) and while its heartbeats
arrive; it is `unreachable` once three heartbeat intervals pass without one, whether its stream
ended or merely went quiet, because a half-open stream and a dead machine look the same from the
control plane. A control plane that starts marks every node it had recorded as connected
`unknown`, since it has heard none of them.

`cordoned` means no new placements; existing workloads keep running. `draining` means migrate
everything off. Both are real states with real behaviour, not admin conveniences; they are what
makes hardware maintenance possible without downtime.

#### Placement

> The binding of a workload to a node.

```
Placement
  workload      WorkloadID
  node          NodeID
  intent        IntentID        the approved intent it was bound for
  reserved      Resources       that intent's reservations, held on the node
  devices       []DeviceHold    the node's devices it holds, each an id and a mode, chosen when
                                it was bound (§6.3 of 05-scheduling.md); added 2026-10-25
  cores         []int           the node's cores it holds, by id, chosen when it was bound for
                                dedicated cores, whose logical CPUs' millicores `reserved` then
                                counts (§5.2 of 05-scheduling.md); added 2026-10-25
  tier          IsolationTier   the tier it runs at, from the target it was bound with
  region        string          the node's `region` label, which every reader of the workload
                                sees though not every reader may see the node (§8 of
                                08-identity.md)
  bound_at      Timestamp
  bound_by      scheduler | operator | migration | recovery
                                recovery: a restore, to the node that presented the workload's
                                assignment (§8 of 03-state.md)
  recovered_epoch  int?         the highest lease epoch a node holding it presented to a restore
  held_at       Timestamp?      a restore held it: its node presented its assignments without it,
                                so nothing is sent for it until an operator confirms it or it is
                                retired (§8 of 03-state.md)
  reason        string          human-readable explanation of why this node
  explanation   Explanation     the full derivation (§2.3 of `05-scheduling.md`)
  authoritative bool
```

**Invariants**
- A persistent workload has at most one active placement.
- During migration two placements coexist, exactly one marked authoritative, until cutover (Rule
  K-4).
- Every placement records **why**. A scheduler that cannot explain its choice is a scheduler nobody
  trusts. Detail in `05-scheduling.md`.
- A node's `allocated` is the sum of `reserved` over its placements, authoritative or not: during a
  migration both nodes hold the workload's capacity.
- A device is held in `passthrough` by at most one placement, counting every device that is the
  same hardware, and in `timeslice` by at most its `shares` (§6.3 of `05-scheduling.md`).

---

### 3.5 Definition

#### Recipe

> A signed, versioned, content-addressed package describing how to run a class of workload. The
> replacement for the egg.

```
Recipe                            an OCI artifact (§2 of 09-recipes.md)
  ref           name + version    e.g. korpis.io/minecraft/paper:1.21.4; a mutable pointer
  digest        sha256            the manifest's content address; the true identity
  document                        the one document layer (§3 of 09-recipes.md):
    metadata      name, version, description, maintainers, license, annotations
    runtime       targets per tier and architecture (an image by digest, or a file of the
                  artifact), and the entry command
    preset        PresetSpec      default lifecycle, console, endpoints, volumes, resources
    config        ConfigSchema    typed settings with constraints and per-field permissions
    install       []InstallStep   restricted DSL, fetch(url, sha256) / extract / template / chmod
    templates     []TemplateSpec  config file rendering, sandboxed variable set
    probes        Probes          readiness and liveness appropriate to the shape
    logs          LogRules        patterns that become events
    lifecycle_hooks               what to do before a stop, such as a console write
    disruption    Disruption      what this software costs to interrupt (§5.1 of 09-recipes.md)
  files                           the other layers, named by their paths in the artifact
  signatures                      not part of the artifact: cosign signatures beside it in the
                                  registry (§7 of 09-recipes.md)
```

> Corrected 2026-10-13, in the audit noted at the head of this section. The first version gave a
> recipe one `image`, which the per-tier targets of §3 of `09-recipes.md` replaced, had no
> `logs`, `lifecycle_hooks`, or `disruption`, and listed `signatures` as a field. A signature cannot
> be a field of what it signs: it is made over the digest, after the artifact exists, and lives
> beside it. What an intent records of it is `RecipeRef.trust`, the accepted signer that vouched or
> the fact that none did (§7 of `09-recipes.md`).

**Invariants**
- Identity is the **digest**, not the name. `name:version` is a mutable pointer resolved through a
  lockfile at declaration time; the intent stores the digest. The same intent produces the same
  bytes forever (Rule K-15).
- `install` steps come from a restricted vocabulary. **There is no step that runs arbitrary code.**
  Every fetch declares its expected hash. This is the direct answer to eggs being bash scripts that
  download from the internet at install time, non-reproducible and a supply-chain hole.
- `templates` render in a sandbox with an explicitly allow-listed variable set. There is no ambient
  scope (Rule K-2; this is the exact mechanism behind critical advisory GHSA-pfvc-3p5h-x7h6, where
  egg templating could be made to render the daemon token and registry credentials into a
  tenant-readable file).
- `config_schema` drives generated forms, server-side validation, and per-field permissions. Raw
  environment variables and a raw text editor are not the primary interface (Rule K-17).

Distribution is through OCI registries: content addressing, signing, mirroring, and CDN already
exist there. Korpis does not run a store (§2 of `00-overview.md`).

Detail in `09-recipes.md`.

---

### 3.6 Attached resources

#### Volume

> Durable state with a lifecycle independent of the workload using it.

```
Volume
  id            VolumeID
  project       ProjectID
  name          string
  size          bytes
  class         StorageClass     local-zfs | local-btrfs | local-ext4 | network | ...
  node          NodeID?          null for network-backed classes
  attached_to   WorkloadID?
  mode          rw | ro | shared
  snapshots     []SnapshotRef
  created       Timestamp
  deleted_at    Timestamp?       soft, as for a workload (§5)
```

As built, the object also records why it exists and what its node says of it: `kind` (`declared`,
or `stream` for a console stream's volume, §4 of `14-streams.md`), `created_by` (the workload it
was made for, which `attached_to` may no longer name once another adopts it), `quota_enforced`
(whether `size` is enforced in the write path; where it is not, `size` is shown nowhere, Rule K-3),
and `detached_at`. The control plane keeps it from the volume events nodes journal, so it never
claims a volume no node made (§7 of `06-storage.md`).

**Invariants**
- A volume outlives the workload attached to it. Deleting a workload does not delete its volumes;
  that is a separate, explicitly irreversible plan step.
- The storage class determines snapshot capability, and snapshot capability determines backup
  strategy (Rule K-5). Choosing ext4 is choosing project quotas and slow backups; choosing ZFS is
  choosing instant snapshots and cheap incrementals. The model surfaces that choice rather than
  hiding it.

#### Snapshot

> A point a volume can be rolled back to, on the volume's node and filesystem. Not a backup.

```
Snapshot
  id            SnapshotID
  volume        VolumeID
  name          string           unique among the volume's live snapshots
  created       Timestamp        when its node took it
  deleted_at    Timestamp?       marked, never removed
```

Added 2026-10-14, when snapshots were built; `Volume.snapshots` lists a volume's live ones. A
snapshot is taken, rolled back to, and deleted by plans of the workload holding its volume, or of
the one that created it when none holds it (§4 and §7 of `06-storage.md`), and the control plane
keeps the object from what the node reports it did.

**Invariants**
- A snapshot survives neither its node nor its filesystem (§4 of `06-storage.md`), and nothing
  presents one as a backup.
- Rolling back loses what was written since, so it is irreversible and always waits for a person;
  so does deleting a snapshot. Taking one changes nothing a workload sees, and applies at once.
- A snapshot goes with its volume: deleting the volume deletes its snapshots.

#### Repository

> Where an organization's backups are kept, and under whose key. §5.3 of `06-storage.md`.

```
Repository
  id            RepositoryID
  organization  OrganizationID
  project       ProjectID?       the whole organization when null
  name          string           unique among the organization's live repositories
  provider      string           the organization's backup target provider, by name
  prefix        string           its place in the target, ending in '/'
  key_custody   operator | tenant
  kms           TenantKey?       under tenant custody: the key_management provider and the key
  retention     RetentionPolicy
  created       Timestamp
```

Added 2026-10-14, when repositories were built. Its key-encryption key is not a field: the vault
holds it sealed (§5.5 of `06-storage.md`), and no read of the object returns it.

**Invariants**
- A workload's objects in a repository are beneath one prefix of its own, which only that
  workload's jobs write and only the control plane prunes.
- A retention policy keeps at least one backup, and nothing prunes below its `min_retention`.

#### Backup

> One backup of one volume, as the node that ran it reported it.

```
Backup
  id            BackupID
  repository    RepositoryID
  workload      WorkloadID
  volume        VolumeID
  plan          PlanID           the plan that takes it
  parent        BackupID?        whose unchanged files it did not read again
  status        pending | succeeded | failed
  consistent    bool             read from a snapshot, or live
  stats         files, directories, bytes, new chunks and bytes, stored bytes, skipped
  created       Timestamp
  finished      Timestamp?
  deleted_at    Timestamp?       pruned; marked, never removed
```

**Invariants**
- A backup is succeeded only once its record is in the repository (§5.7 of `06-storage.md`), so
  a succeeded backup is whole.
- A backup is authorized on its workload: `backup.read`, `backup.browse`, and `backup.restore`
  there reach that workload's backups and no other's. Restoring one into another workload's
  volume also takes `workload.file.write` on that workload (§5.4 of `06-storage.md`).

#### Endpoint

> A way the network reaches a workload. The replacement for the allocation.

```
Endpoint
  id            EndpointID
  workload      WorkloadID
  name          string           "game", "rcon", "http", "query"
  protocol      tcp | udp | http | https | quic
  container_port  int
  exposure      node_port | node_bound | stable | ingress | dedicated_ip | overlay_only | none
                                 the mode the declared one resolved to for this placement
                                 (§3 of 07-networking.md): `stable` without an edge tier is
                                 `node_bound`
  address       Address?         assigned; shape depends on exposure
  client_address  preserved | translated | proxy_protocol   how the workload sees the client
                                 (§3.4 of 07-networking.md); declared only as proxy_protocol,
                                 on an ingress passthrough endpoint (§3.2 there), added
                                 2026-10-22
  domains       []string         for ingress exposure only: 1 to 100 exact names its
                                 organization holds, each routed by no other endpoint (§3.2
                                 of 07-networking.md)
  tls           TLSSpec?         acme (the default), provided (a Certificate), or passthrough;
                                 for TLS the proxy terminates, the client certificates it asks
                                 for and the authorities it trusts (§3.2 of 07-networking.md),
                                 added 2026-10-22
  certificate   CertificateID?   for ingress the proxy terminates: the certificate it is served
                                 with, once one with a key is held, and that certificate's
                                 revision, so its node fetches it again when it changes
  policy        NetworkPolicy    who may connect to it, and how many at once; a workload's
                                 outbound rules are its own (IntentBody.egress), corrected
                                 2026-10-15 (§6 of 07-networking.md)
  accounting    bool             meter traffic on this endpoint
  shaping       ShapingSpec?     bandwidth limits (§7 of 07-networking.md)
```

> Second external review, finding S16. The first version of this struct had no `stable`, which is
> the exposure mode `07-networking.md` is built around, no `none`, no `quic`, and no `shaping`.
> `07-networking.md` §2 is the detail; this is the list, and the two now agree.

Pterodactyl's `Allocation` is a bare `(ip, port)` row on a node, which is why it has no ingress, no
TLS, no service discovery, no egress control, and no traffic accounting, and why the allocation
rework is still on Pelican's *planned* list years after the fork. `(ip, port)` is load-bearing for
the entire schema, so it cannot be replaced by anyone who inherited it.

`Endpoint` carries the exposure mechanism, the domain, the TLS policy, the network policy, and the
accounting flag as first-class fields. A game server's UDP port and a web app's HTTPS ingress are
the same object with different values (P7). Detail in `07-networking.md`.

#### Domain

> A DNS name an organization has shown it controls, which its endpoints may then route.

> Added 2026-10-15, when the HTTP ingress was designed (§3.2 of `07-networking.md`). An endpoint
> named domains since the first version, and nothing said whose a name is: on a shared ingress, a
> tenant who could route any name could take another's traffic, and obtain a certificate for it.

```
Domain
  id            DomainID         dom_…
  organization  OrganizationID   the holder
  name          string           a lowercase DNS name; covers itself and every name beneath it
  verification  dns_txt | delegated | operator
  token         string           what a TXT record at `_korpis-verify.<name>` must hold
  parent        DomainID?        for `delegated`: the ancestor's domain it lies beneath
  verified_at   Timestamp?       null while pending
  created_at    Timestamp
```

**Invariants.** A verified domain of one organization neither covers nor lies beneath a verified
domain of another, except one delegated beneath its parent, whose holder is an ancestor of its own
holder. A pending domain blocks no one, so a name cannot be squatted by asking for it. An endpoint
names only names its own organization holds, the holder of a name being the organization whose
verified domain is the most specific one covering it. Deleting a domain deletes the domains
delegated beneath it.

> Corrected 2026-10-15, when domains were built: the last invariant said an endpoint names only
> names a verified domain of its own organization covers, which let an organization route the
> names it had delegated to a descendant, its own domain covering them still (Finding 59 of
> `23-walkthroughs.md`).

#### Certificate

> A certificate and its key, for the names of an endpoint the ingress terminates.

```
Certificate
  id            CertificateID    crt_…
  organization  OrganizationID
  names         []string         the names it is for; a wildcard covers one label
  source        acme | provided
  chain         PEM              leaf first
  key           sealed           the private key, sealed by the control plane's TLS key
  not_before, not_after  Timestamp
  state         pending | issued | failed
  reason        string?          why it failed, for `failed`
  renew_after   Timestamp?       for `acme`: when its renewal begins
  revision      int              raised whenever its chain and key change
  endpoint      (WorkloadID, string)?  for `acme`: the endpoint whose names it is for
  portal        PortalID?        for `acme`: the portal whose name it is for, added 2026-10-21
  attempts      int              for `acme`: attempts in a row that failed
  next_attempt  Timestamp?       for `acme`: when the next may be made
```

**Invariants.** Its key is stored only sealed (§3.2 of `07-networking.md`). An `acme` certificate
is the control plane's to issue and renew, for one endpoint's names or one portal's name; a
`provided` one is uploaded by its organization and named by the endpoints that use it
(`TlsSpec.secret` holds its identifier, §3.2 of `07-networking.md`), which must be its
organization's and covered by it, and is replaced in place by its organization, keeping its
identifier, raising its revision; a portal names one too. A certificate an endpoint or a portal
serves with is not deleted: the endpoint's routes, or the portal, name it.

#### Portal

> A name an organization holds, under which the control plane serves its API and panel for it.

> Added 2026-10-21, when the control plane was designed to answer under names organizations hold
> (§3.6 of `07-networking.md`, §9.8 of `08-identity.md`), resolving Finding 66 of
> `23-walkthroughs.md`.

```
Portal
  id            PortalID         ptl_…
  organization  OrganizationID   whose theme and identity providers it wears, and beneath which
                                 a session begun at it acts
  name          string           a lowercase DNS name its organization holds; the relying
                                 party's identifier for every passkey registered at it
  certificate   CertificateID    what it is served with: its own `acme` one, or a `provided` one
                                 of its organization covering its name
  created_at    Timestamp
```

**Invariants.** Its name is held by its organization, the holder being the organization whose
verified domain is the most specific one covering it, and while the portal exists that domain is
neither deleted nor delegated away. A name is a portal or an endpoint's route, never both, and two
portals never share one; no portal is the installation's own host or a `--web-origin`'s. A passkey
registered at a portal is used only there, and deleting the portal removes its passkeys and ends
the sessions begun at it.

#### Stream

> A durable, addressable channel of output from a workload.

```
Stream
  id            StreamID
  workload      WorkloadID
  run           RunID?           the run whose output it is, for a task or scheduled workload's
                                 console (§4.5 of 05-scheduling.md); added 2026-10-18
  kind          console | log | event | metric
  retention     Duration | bytes
  segments      []Segment        content-addressed, offloadable to object storage
  node_only_bytes  bytes         held only on the node, not yet offloaded (§4 of 14-streams.md)

Segment
  first_offset, end_offset       the records it holds, by offset (§2 of 14-streams.md)
  first_at, last_at  Timestamp   node clock; informational, as offsets order
  bytes         bytes
  digest        sha256?          its address once sealed
  node          NodeID?          the node holding it, while one does
  repository    RepositoryID?    the repository holding it once offloaded (§4 of 14-streams.md)
```

> Corrected 2026-10-14, when stream reading was built. The model gave a stream an `indexed bool`
> that no document defined, and §8 of `14-streams.md` decides the opposite of an index: a stream is
> searched by scanning the node's segment files, within its retention, because search is a
> property of those files and not a search product. A field with nothing to mean was a promise
> nothing kept, so it is gone, and its wire field number is reserved.

> Corrected 2026-10-14, when reading offloaded segments was built. A segment's `offloaded bool`
> said that a copy existed and not where, so nothing could fetch it once its node was gone, which is
> the one case offload is for. It names the repository now; the wire keeps `offloaded` beside it.

The agent seals and offloads segments and journals each (§4.10 of `02-architecture.md`); the
control plane keeps the index from those effects. That is what lets an offloaded segment be found
after its node is gone, which is the one failure §4 of `14-streams.md` says offload covers.

**Invariants**
- Streams are durable and seekable, not ephemeral websocket pipes. Reconnecting resumes at an
  offset; it does not restart from an in-memory ring buffer.
- A stream can be shared by grant: a bounded time range, read-only, to a subject who has no other
  authority. Attaching a crash log to a support ticket is a grant, not a copy-paste.
- Streams outlive the workload. A crashed workload's final output is still there after it is gone.
- **Durability is stated per failure, not in general.** A stream survives the workload crashing, the
  agent restarting, and the workload migrating (§4 of `14-streams.md`). It survives the **node**
  being lost only for segments already offloaded, so offload is on by default wherever an offload
  target exists (a repository that takes streams, §4 of `14-streams.md`), and the interface shows
  how much of a stream is held only on its node.

> Second external review, finding S12. Recorded in §17 of `23-walkthroughs.md`. The first version
> said streams were durable and outlived the workload without qualification, while §4 of
> `14-streams.md` kept segments on the node and offloaded them only "if configured". A node that
> died took its unoffloaded segments with it, and the invariant claimed otherwise.

Detail in `14-streams.md`.

---

### 3.7 Extension

> A separate process that adds capability, holding a scoped grant and speaking a versioned
> contract.

```
Extension
  id            ExtensionID      the installation; its subject is `extension:<id>`
  name          string
  version       SemVer
  api_version   string           the contract version it speaks
  organization  OrgID            where it is installed
  project       ProjectID?       when installed into one project rather than the organization
  workload      WorkloadID       the workload it runs as (§3 of 16-extensions.md)
  artifact      RecipeRef        its artifact, by digest, and what vouched for it
  grants        []GrantID        one per approved need, each attenuated from the installer's
  provides      []{type, name}   what it may register to serve
  subscribes    []EventFilter
  routes        []RoutePrefix    served under /ext/<name>
  ui_slots      []SlotSpec       declared mount points in the web client
  installed_by  Subject
  status        running | stopped | failed | incompatible
```

> Corrected 2026-10-13, while building the contract. The first version had one `grant`, while
> installation issues one per approved need (§4 of `16-extensions.md`), and nothing tied an
> extension to its workload, its artifact, or where it was installed. Its subject was keyed by
> name, `extension:<name>`, so the same extension installed into two organizations would have been
> one subject holding both installations' grants; the key is now the installation's identifier.

**Invariants**
- An extension **cannot modify Korpis' own files.** It is a process with a token, not a script with
  filesystem access (Rule K-6, the direct answer to the Blueprint framework's `sed`-patching of
  Pterodactyl core files).
- An extension's effects appear in the effect stream attributed to its grant, exactly like a
  human's.
- Its workload runs only the artifact digest it was installed from, or was upgraded to through the
  extension service, and an extension's artifact runs only as an installation, never as a plain
  workload (§4 and §9 question 6 of `16-extensions.md`).
- Core features use this same mechanism. If the contract is insufficient for something core needs,
  the contract is extended, core is never given a private door (P8).

Detail in `16-extensions.md`.

---

## 4. How it fits together

```
Organization ──┬─▶ Organization        (recursive: this is reselling)
               │
               ├──▶ Quota              (inherited, enforced, metered)
               │
               └──▶ Project ──▶ Workload
                                  │
                                  ├──▶ DeclaredSet?                 (the files that own it)
                                  │
                                  ├──▶ Intent  ──▶ Intent ──▶ …   (immutable chain)
                                  │       │
                                  │       └──▶ RecipeRef ──▶ Recipe @digest
                                  │
                                  ├──▶ Observation                 (written by the agent)
                                  │
                                  ├──▶ Placement ──▶ Node
                                  │
                                  ├──▶ Volume[]     Endpoint[]     Stream[]
                                  │
                                  └──▶ Plan ──▶ Effect[]           (append-only)

Subject ──▶ Grant ──▶ Grant ──▶ …        (attenuation chain; every Effect names one)
   ▲
   └── User | Token | ExternalIdentity | Workload | Extension
```

Two loops define the system:

**The convergence loop.** An `Intent` or a `Power` change is declared → the control plane computes a
`Plan` → the plan is approved and `Workload.target` moves → the workload's assignment changes → the
agent converges to it, executing any node steps, and journals `Effect`s → the agent reports
`Observation` → the control plane compares observation against assignment → drift is corrected
toward the approved target without a new plan, and a new declaration starts the loop again. The
loop never stops, and nothing outside it changes durable state.

**The authority chain.** Every `Effect` names the `Grant` that permitted it. Every `Grant` names
its parent. Follow the chain from any action taken in the system to a root grant, and you have the
complete answer to *who allowed this, and who allowed them*.

---

## 5. Naming and identity rules

1. **IDs are immutable and never reused.** Names are mutable and may be reused. Nothing internal
   references anything by name.
2. **Deletion is soft at the identity layer.** IDs persist forever so that effects, streams,
   backups, and metering records are never reattributed to a different object.
3. **Content-addressed things are identified by digest.** Recipes, stream segments, backup chunks.
   Human-readable names are pointers resolved at declaration time and recorded as digests.
4. **No object carries a `type` discriminator that changes behaviour in core.** Behaviour comes
   from declared capabilities, a driver's `DriverInfo`, a recipe's `preset`, an extension's
   `api_version`. Core asks; it does not switch on kind.

---

## 6. What is deliberately absent

- **No `Server`.** The word does not appear in the model. It is ambiguous between the machine and
  the workload, and it is what trapped Pterodactyl in the game vertical.
- **No `Location` or `Nest`.** Labels and selectors.
- **No `Allocation`.** `Endpoint`.
- **No `Egg`.** `Recipe`, content-addressed and signed.
- **No `Role`.** `Grant`.
- **No `is_admin` flag.** A wide-scoped grant.
- **No `spec`/`status` on one object.** `Intent` and `Observation`, separate writers.
- **No game concept anywhere.** Query protocols, RCON, modpacks, wipes, and map rotation are recipe
  and extension concerns (§2 of `00-overview.md`).

---

## 7. Open questions

Recorded rather than resolved, and each settled in the document named. All eight are now resolved.
They are kept as they were asked, with the answer and its location in front, because the question
is the record of what this model did not originally say.

1. **Does `Plan` cover multi-workload changes? Resolved in §3 of `05-scheduling.md`**: a `Plan`
   covers one workload, and an `Operation` owns many. Draining a node touches many workloads. Is
   that one plan with many targets, or many plans under a parent operation?
2. **What is the storage engine for `Effect`? Resolved in §1 and §4 of `03-state.md`**: an
   append-only, time-partitioned PostgreSQL table, without event sourcing. Postgres append-only
   table, or a separate log with Postgres as projection? The answer determines whether cheap
   time-travel is available. Note that an earlier design proposal built the job queue on
   `LISTEN/NOTIFY` and `SKIP LOCKED` while also claiming to keep a SQLite path open, those are
   contradictory and the contradiction must be resolved here, not deferred.
3. **Can a `Grant` name a set of workloads by label selector, or only by explicit ID? Resolved in
   §3.4 of `08-identity.md`**: yes, always bounded by an explicit container, with membership
   materialized. Selectors are far more useful and considerably harder to reason about when the
   set changes after issue.
4. **Where do `task` and `scheduled` workloads keep their run history? Resolved in §4 of
   `05-scheduling.md`**: a separate `Run` object. A `scheduled` workload produces many runs. Are
   runs workloads, or a separate `Run` object?
5. **Does `Quota` allow overcommit? Resolved in §5 of `05-scheduling.md`**: limits may exceed the
   parent's by a declared per-dimension ratio; reservations never do; memory overcommit is off by
   default. Hosting providers depend on it. Which dimensions may be oversubscribed, and what happens
   under contention?
6. **How are `Observation`s retained? Resolved in §5 of `03-state.md`**: the latest in place,
   history downsampled in the telemetry path, and state transitions promoted into the effect log.
   This was routed to `15-observability.md`, which never carried it (finding S18). Every observation
   forever is unaffordable; only the latest loses the history that makes incidents explicable.
7. **Can a `Quota` be scoped to a label selector? Resolved in §5.1 of `05-scheduling.md`**:
   `QuotaSet`, a set of selector-scoped entries, every matching one enforced. As written, `Quota`
   was a flat resource bound per organization. A hosting provider sells "4 GB in the EU", capacity
   in one region is not interchangeable with capacity in another, so a single global bound cannot
   express what is actually being sold.
8. **How does one workload address another? Resolved in §5 of `07-networking.md`**. The model gave a
   workload `Endpoint`s and an `overlay_only` exposure, but no name resolution. A web service needs
   to reach its database without either knowing the other's placement or hard-coding an IP that
   changes on migration.
