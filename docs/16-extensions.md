# Extensions

**Status:** design **Date:** 2026-08-07 **Depends on:** [`08-identity.md`](./08-identity.md),
[`10-api.md`](./10-api.md), [`09-recipes.md`](./09-recipes.md) **Implements:** Principle P8, Rule
K-6

---

## 1. Why this is not optional

Pterodactyl has no extension API. The ecosystem that grew anyway standardized on patching core PHP
files with `sed`, an approach that breaks on every upgrade, cannot be audited, cannot be
uninstalled cleanly, and makes the panel's own release cadence a hazard to its users.

That is not a community failing. It is what people do when the alternative is not existing. The
extension interface has to be designed before there are consumers for it, because after there are
consumers, the patching convention is already load-bearing.

---

## 2. Core uses the same mechanism

P8's falsifiable claim: **if the extension interface is not sufficient for core features, it does
not ship.** These are extensions in Korpis, not core code with an extension API bolted beside it:

| Extension | Point |
|---|---|
| game query protocols (Source, Minecraft, GameSpy) | health provider |
| RCON, and console adapters for non-stdout consoles | console provider |
| Steam app installation (`steam.app` install step) | recipe step provider |
| DNS providers (Cloudflare, Route53, RFC 2136) | DNS provider |
| object-storage backup targets | backup target provider |
| OIDC, SAML, LDAP, Discord identity sources | identity provider, in the trust base (§5.3 of `08-identity.md`) |
| protocol-aware routers (Minecraft handshake, TLS SNI) | edge router provider |
| alert routing beyond the built-in minimum | event consumer |
| **the Discord client**: and the chat-adapter interface behind it | actions + events + streams |

Every one of these was previously named in another document as "an extension" precisely so that
this list could be checked rather than asserted. If any of them turns out to need a core change,
the interface is wrong and the interface gets fixed, not the exception granted.

---

## 3. An extension is a workload

Korpis runs itself.

An extension is packaged as an OCI artifact, distributed and signed exactly like a recipe (§2 of
`09-recipes.md`), and executed as a `Workload` on a node, with an isolation tier, a resource
reservation, declared egress, and a lease. It has a `Subject` and holds grants. It cannot read the
database, cannot open a socket the control plane trusts implicitly, and cannot touch a Korpis file.

This is not cleverness for its own sake. It means an extension gets the entire platform's
isolation, scheduling, observability, quota, and audit machinery for free, and it means a
misbehaving extension is a misbehaving workload, a category the system already knows how to
contain, throttle, restart, and evict.

It also means the answer to "can an extension take down my control plane" is the same as the answer
to "can a tenant's game server take down my node", which is a question this design has already
spent `04-runtimes.md` and `17-security.md` answering.

> Added 2026-10-13, while building the contract in Phase 1. This section said an extension is a
> workload with a subject and grants, and nothing said what its artifact contains, how it proves
> it is that subject, or how Korpis reaches it when nodes accept no inbound connection (§3 of
> `02-architecture.md`). §3.1 to §3.3 answer the three.

### 3.1 What an extension is made of

**An extension is a recipe with one more section.** Its artifact has the layout of §2 of
`09-recipes.md`, with artifact type `application/vnd.korpis.extension.v1` and a document of media
type `application/vnd.korpis.extension.document.v1+yaml` whose `kind` is `Extension`. Everything a
recipe document holds describes the extension's workload; the `extension` block describes the
extension:

```
extension:
  api_version   korpis.extension.v1     the contract it speaks (§6)
  requires      []Capability            contract capabilities it cannot work without (§6)
  needs         []Need                  actions, and the scope each is wanted over (§4)
  egress        []host:port             what it reaches; its workload's egress policy
  subscribes    []EventFilter           what it consumes (§5)
  provides      []{type, name}          what it serves, `dns` / `cloudflare` (§5)
  actions       []ActionDecl            `ext.<name>.<action>`, Phase 4 (§5)
  ui_slots      []SlotDecl              Phase 4 (§5)
```

**How the block is written and checked** (2026-10-13, from the implementation). A need is
`{actions, scope, reason}`: `scope` is `target`, `organization`, or `self` (its own workload), and
`reason` is required, because it is the line the installer reads before approving. An action is
needed over a scope once. `egress` entries are `host:port`, an IPv6 host in brackets. A
subscription is `{kinds, min_severity, workloads}`, the filter of §5 of `15-observability.md`
without its projects: a document cannot know the projects of the organization it will be
installed into, so the installation narrows them. A document that subscribes must need
`event.read`, since delivery reaches only what that grant reaches, and a subscription without it
would install cleanly and deliver nothing, forever. `provides` entries are `{type, name}`, the
type one of `dns`, `backup_target`, `step`, `health`, `console`, `identity`, `edge_router`,
`chat_adapter`, the name a lowercase word, and for `step` `<publisher>/<step>`. The extension's
name, its document's `metadata.name`, is a lowercase word, because it names its actions
(`ext.<name>.<action>`). A document of kind `Recipe` with an `extension` block, or of kind
`Extension` without one, does not load. Everything else in the document is read as any recipe's,
by the same parser, so a driver preparing the extension's workload reads it unchanged.

One format means one signing path and one trust policy: an extension is admitted by the trust
policy that admits recipes (§7 of `09-recipes.md`), and nothing about extensions needs a second
answer to "whose signature do we run". The cost is that an extension author writes a recipe, which
is the one thing every Korpis workload author already writes.

### 3.2 How it proves who it is

It is a workload, so it proves it the way every workload does: its node vouches for it, through
the identity socket of §9.5 of `08-identity.md`, and the control plane authenticates it as
`extension:<id>`, the subject the installation's grants were issued to. It holds no secret that
outlives its placement, and an uninstalled extension's workload can obtain nothing, because its
grants are revoked and its workload retired.

### 3.3 How Korpis reaches it

**The extension dials out, and Korpis calls it down the connection it opened.** A provider opens
`ProviderService.Serve`, a bidirectional stream to the control plane, authenticated by its
workload credential. Its first message registers what it serves, which must be one of the
`provides` entries its installation recorded. The control plane then sends calls down that stream,
each with an identifier and a deadline, and the results come back on it. Several replicas of one
provider register several streams and calls are spread across them; a stream that ends withdraws
its registration, and a call in flight on it fails as retryable.

This is the agent's connection direction (§3 of `02-architecture.md`) for the same reason: the
party that runs on a node dials out, so a node needs no inbound port and an extension needs no
address the control plane must reach. What it costs: the control plane holds one open stream per
provider replica, and a provider whose stream has dropped is unavailable until it reconnects,
which the breaker of §5 treats like any other failure.

---

## 4. Permissions are grants, declared and granted at install

An extension declares what it needs. The operator sees the list and approves it. From then on it
acts as itself, in the audit trail, under grants that attenuate like everyone else's.

```
extension: cloudflare-dns
  needs:
    endpoint.read        scope: organization
    dns.record.write     scope: zones it is configured for
    event.read           filter: endpoint.*
  egress:
    api.cloudflare.com:443
```

**What installation does.** An install names the extension's artifact, the organization or
project it is installed into, the project its workload runs in, and the needs the installer
approves. The control plane resolves the artifact under the trust policy, checks that the installer
may install it there (§4.1), and issues one grant per approved need to `extension:<id>`, each
attenuated from a grant the installer holds that covers it, which becomes its parent. A need no
grant of the installer's covers is refused by name, before anything is created. Then the
extension's workload is declared from its document, its subscriptions are recorded, and its
`provides` entries become registrations its streams may claim. Uninstalling revokes those grants,
which revokes everything descended from them, ends its subscriptions and registrations, and
proposes the retirement of its workload. Installing needs `extension.install`, uninstalling
`extension.uninstall`, and reading what is installed `extension.read` (§5 of `10-api.md`).

> Added 2026-10-13, with §3.1 to §3.3. The example above and the paragraphs after it described
> what an installer sees and not what installing does, so nothing said where an extension's grants
> come from in the chain that §3 of `08-identity.md` requires every grant to descend through.

This is the phone-application permission model, and it works here for the reason it fails in most
server software: grants already exist, so there is nothing to invent. A user can see what an
extension may do by reading its grants, revoke one without uninstalling, and read every action it
took in the same `Effect` log everything else writes to (§4 of `15-observability.md`).

An extension can never hold authority its installer did not have, because a grant only produces
weaker children (P6). A reseller installing an extension inside their own organization cannot
thereby obtain reach into a sibling organization, and this requires no check; it is structurally
impossible.

### 4.1 Who may install one, in a delegated tree

> Finding 7 of `23-walkthroughs.md`.

With three levels (operator, reseller, customer) "the operator approves" names three different
people. The instinct is to add an approval object. That instinct is wrong, and the reason is worth
stating because it recurs:

**"It runs code on someone else's machine" proves too much.** Tenants already run arbitrary code;
that is the product. A Minecraft server with forty forum-sourced plugins is arbitrary code, and an
extension is a workload in the same isolation tier, under the same quota, subject to the same
egress policy. From that angle an extension is not more dangerous than what the platform exists to
run.

The real distinction is **which direction the call goes**:

> An extension that Korpis *calls* is infrastructure. An extension that calls Korpis is a tenant.

| Kind | Whose exposure | Who may install |
|---|---|---|
| event consumer, action, UI slot | the installer's own data and quota | **the installer**, their grants already suffice |
| **provider**: DNS, backup target, edge router | the control path; Korpis calls it, waits on it, depends on it | **whoever owns the resource it provides for** |
| **identity provider** | authentication itself: whatever it asserts is believed | **whoever holds authority over the whole organization it authenticates into** |
| **recipe step, health, and console providers** | the install path and the probes of every workload whose recipe names them | **whoever holds authority over the organization** whose recipes may use them; they serve that organization's subtree |

> Second external review, finding S26. Recorded in §17 of `23-walkthroughs.md`. This table named
> DNS, backup, identity, and edge providers, and none of the providers §2 lists first: the recipe
> step, health, and console providers. Nothing said who may register `steam.app`, how its name is
> kept from colliding, or which registration a recipe gets.

**Step providers are named, scoped, and pinned.** A step kind is named `<publisher>/<step>`, so two
publishers' `app` steps cannot collide, and recipes write `valve/steam.app` where they used to write
`steam.app`. A registration belongs to an organization and is visible to its subtree; two
registrations of one name in one organization are refused, and a child organization's own
registration shadows its parent's for that child's subtree only, visibly. When a recipe is
declared, the step kinds it uses are resolved against the registrations visible to the declaring
organization and the provider's **artifact digest is recorded in the lockfile** beside the recipe's
own (§8 of `09-recipes.md`), so a workload never changes provider because someone registered a new
one later.

Tenant-direction extensions need nothing new. Authority is bounded structurally, consumption is
metered against the installer, and egress obeys the tenant's egress policy, so an operator who
forbids arbitrary egress has already stopped the extension from reaching Cloudflare. The lever
exists.

Provider-direction extensions need no new object either, because the answer is **scope**: a DNS
provider is installable by whoever owns the zone, a backup target by whoever owns the repository,
an identity provider by whoever owns the organization it authenticates into, an edge router by
whoever owns the edge. A reseller runs a DNS provider for their own zones and cannot place code in
the operator's forwarding path.

Separately and optionally, an operator may keep a **publisher or registry allow-list**, the same
mechanism §13.4 of `17-security.md` proposes for recipes, default open. A shared-hosting provider
may not want customers installing arbitrary extensions at all; that is their policy to set, not our
default to impose.

---

## 5. Four extension points

### Event consumers

Subscribe to the event stream, filtered by grant. Delivery is at-least-once with a persistent
outbox; consumers are expected to be idempotent, and the delivery identifier makes that easy. A
slow or dead consumer's queue is bounded and is dropped with a visible marker rather than growing
until it becomes the control plane's problem.

**Delivery is ordered per workload.** Every event carries a sequence number scoped to the workload
it concerns, the outbox delivers each workload's events in that order, and a consumer that sees a
gap in a sequence knows it, rather than inferring order from timestamps. What an event is, where it
is written, and how a subscription keeps its position are in the event log of §5 of
`15-observability.md`; an extension's subscriptions are the `subscribes` entries of its document,
recorded at install, and deliver only what its `event.read` grant reaches. Across workloads there is
no order, because none is meaningful and providing one would make one slow workload's events hold
up everyone else's. This answers the question §12 of `10-api.md` routed here (finding S18).

### Providers

Korpis calls **out** to the extension, down the stream of §3.3, to obtain a capability it does not
implement: resolve a DNS record, run a health probe, install a Steam application, write a backup to
an object store, verify an identity assertion, route a protocol.

Calling out is the direction that carries risk, so the contract is strict:

- Every provider call has a deadline. There is no unbounded wait on third-party code.
- Failure behaviour is **declared per provider type**, never guessed. A DNS provider that is
  unreachable does not block a workload from starting; the endpoint's observation becomes
  `unsatisfiable` with the reason attached, and reconciliation retries, the same treatment §7 of
  `05-scheduling.md` gives every other unsatisfiable condition. An identity provider that is
  unreachable fails closed, because failing open there is an authentication bypass.
- Providers are circuit-broken. A provider failing consistently is marked down, its dependents are
  marked `unsatisfiable` with the provider named, and it is retried on a backoff, instead of every
  reconciliation loop in the fleet queueing behind it.
- **The breaker trips on latency as well as on failure**, and provider concurrency is bounded per
  provider rather than per call.
- **A step provider states what it produced and whether it was knowable in advance.** It returns
  content-addressed artifacts, never writes into a tenant's volume itself (the workload's own
  filesystem worker copies them in, §4 of `09-recipes.md`), and declares per call whether its
  output was **pinned** (the request named the content exactly, a Steam depot manifest for
  instance) or **unpinned** (the request named a channel and got whatever it currently holds). An
  unpinned result marks the workload non-reproducible, exactly as an unverified egg does (§9 of
  `09-recipes.md`). The contract also carries the provider's argument schema, its idempotence
  declaration (§4 of `09-recipes.md`), and its failure reasons, which surface in the plan as the
  failed step's reason. This is the contract §10 of `09-recipes.md` routed here and that this
  section had not yet written down.

> Finding 23 of `23-walkthroughs.md`.

The last point exists because the first three all describe a provider that is broken, and the
expensive failure mode is a provider that works. A DNS provider answering correctly at ninety per
cent of a five-second deadline, every time, for every workload in the fleet, produces no error to
trip anything while consuming the reconciler's throughput. **A deadline bounds one call and says
nothing about aggregate cost.**

So latency is measured and published per provider, sustained latency trips the breaker the same way
sustained failure does, and a bounded concurrency per provider means one slow dependency occupies a
slice of reconciliation capacity rather than all of it. `15-observability.md` §5 carries the
`provider.degraded` event, because the operator whose creates got slow this morning should not have
to infer the cause from a graph.

The tenant boundary already holds here and is worth naming: a provider installed by one tenant is
bounded by the grants it was installed with (§4.1), so a slow tenant-installed provider degrades
that tenant's own reconciliation and nobody else's. The fleet-wide case is a provider installed at
the operator level, which is exactly what §2 means by core using the same mechanism, and it is the
price of that decision rather than an accident.

### Actions and commands

An extension registers actions in the `ext.<name>.<action>` namespace (§5 of `10-api.md`). Because
the web, Discord, and CLI surfaces are generated from the schema, a registered action **appears in
all three automatically** (as a button, a slash command, and a subcommand) with autocomplete,
authorization, Plan rendering, and audit already attached.

**An extension acting for someone acts with the intersection, never with its own grant.**

> Second external review, finding S25. Recorded in §17 of `23-walkthroughs.md`.

The gateway checked the invoker's `ext.rcon.kick`, and the extension then did the kicking under its
own, broader grant, with nothing tying the resource it touched to the resource the invoker was
authorized for. A user authorized on workload A could pass workload B. That is the confused deputy,
and the structure has to make it impossible rather than ask extension authors to avoid it:

- **An action's schema declares which of its arguments are resources**, typed (`workload:
  WorkloadRef`). The gateway authorizes the invoker for the action **on each declared resource**.
- **The extension receives an invocation token, not the invoker's token and not a blank cheque.**
  It is bound to the invocation, expires with it, and permits only the core actions the extension's
  own grant permits, **on only the resources the invoker was authorized for**. Calls the extension
  makes for that invocation must carry it; an effect made with it names the invoker, the extension,
  and the invocation.
- What the extension does on its own initiative, under its standing grant (a scheduled sync, a
  background probe), is attributed to the extension alone and never to a user who happened to
  invoke something earlier.

This is the payoff for P2 being enforced rather than promised. In every competing product, an
extension that wants a Discord command writes a Discord bot.

### UI slots

Declared, typed, and named. An extension contributes to a slot; it does not edit the bundle, and
there is no supported way for it to.

Most contributions are **declarative descriptors**, a form driven by a schema, a table driven by a
query, a status card, a settings section. These render in the panel's own components, so they
inherit its theming, accessibility, mobile layout, and its three-state rendering of unknown values
(§2 of `11-surface-web.md`).

For genuinely custom UI there is an iframe on a **separate origin**, communicating over a typed
`postMessage` channel, under a restrictive CSP. It never receives the user's capability token; it
receives its own, scoped to the **intersection** of the extension's grants and the viewer's
authority, for the resources in the current context. Three things follow that are worth stating:
an extension cannot exfiltrate the user's authority; a viewer cannot borrow the extension's, which
the first version permitted (a share-link holder opening the panel view received a token scoped to
the extension's grants, finding S25); and an extension cannot render a convincing fake login
prompt inside the panel's own chrome, because it is visibly and structurally a foreign frame.

Runtime drivers are deliberately **not** in this list. §3 of `04-runtimes.md` defines them as
separate processes with their own interface, because a driver's privilege union is different in
kind from an extension's, and pretending otherwise would put the two on the same footing at exactly
the point where they must not be.

---

## 6. Versioning

The rules of §4 of `10-api.md` apply unchanged: additive only, no renames, no splits, capability
negotiation rather than version sniffing.

An extension declares the API version and the capabilities it requires. **Incompatibility is
detected at load, refused with a message naming what is missing, and never discovered at first
use**, the alternative being an extension that installs cleanly and fails six weeks later during an
incident.

The support window is defined the same way as the agent protocol's (§4 of `10-api.md`): every
contract version shipped by a release that is still supported, every LTS included. The reason is the
same: extensions live on machines and in registries that Korpis does not control, and an upgrade
that silently orphans an operator's DNS provider is an outage the operator did not choose.

---

## 7. What extensions cannot do

Stated as a list because the boundary is the design:

- read or write the database
- modify, replace, or read Korpis' own files
- hold authority exceeding their installer's
- receive another subject's capability token (acting for an asserted user goes through `InvokeAs`,
  §5.2 of `08-identity.md`, which hands the extension no token at all)
- act, during an invocation, on a resource the invoker was not authorized for
- run in the control plane's process, address space, or privilege
- add a field to a core object: they carry their own state, keyed by core identifiers
- block a reconciliation loop indefinitely
- disable, weaken, or bypass a grant condition
- remove a recipe-declared redaction pattern (§7 of `14-streams.md`)

The last two matter because an extension is often installed by an operator on behalf of tenants,
and the tenant is not always the party being protected from.

---

## 8. Distribution

OCI artifacts in ordinary registries, addressed by digest, signed with cosign, identical to recipes
(§2 and §7 of `09-recipes.md`), for the identical reason: registries already provide hosting,
mirroring, signing, and CDN, and Korpis is not a marketplace (§2 of `00-overview.md`).

Extensions are licensed at their authors' choice (§5 of `00-overview.md`). **Paid, proprietary
extensions are explicitly permitted**, and this is deliberate: P10 removes the platform tax, and P8
gives a commercial host the supported place to build proprietary differentiation. A market where
the platform is free and the specialized extensions are sold is the outcome this design is aiming
at, and it is what the AGPL/Apache split in §5 of `00-overview.md` exists to make possible.

---

## 9. Open questions

1. **Hot reload versus restart. Resolved here.** An extension is a workload, so replacing it is a
   rolling update, which is already solved. Whether a *provider* can be replaced without dependents
   seeing a failure window needs the drain semantics of §3 of `05-scheduling.md` applied to
   provider registration. It was routed to `05-scheduling.md`, which never carried it (finding S18).

A provider replacement registers the new version beside the old one; new calls go to the new
version; calls in flight on the old one finish within their deadline; then the old registration is
withdrawn. Dependents see no failure window, because no call is ever sent to a provider that is not
registered, and a call that fails during the switch is retried under the provider's ordinary
breaker policy. It is a drain, applied to one registration rather than one node.
2. **Extension-to-extension dependency.** A game-query extension and a Discord-notification
   extension composing is natural, and a dependency graph invites version-resolution complexity
   that has eaten larger projects. Possibly the answer is that they compose only through the event
   stream and the API, never directly. → here
3. **CLI plugins.** Carried from §9 of `13-surface-cli.md`. `korpis-foo` on `PATH` is cheap and is
   a path-hijacking vector. → here
4. **UI slot inventory.** Which slots exist is a real design surface, too few and extensions patch
   around them, too many and the panel cannot be restyled. It should be driven by what the core
   extensions in §2 actually need, and no slot should be added without a consumer. →
   `11-surface-web.md`
5. **Chat adapters. Resolved: extensions, and the Discord client is one of them.** Settled in §10
   of `12-surface-discord.md`. It is the strongest single test of §2's claim (an extension that is
   a complete client, not a peripheral) which is why it is worth building through the contract even
   though building it in-core would be easier. What remains genuinely open is how much of the
   widget vocabulary the adapter interface can carry across platforms, and that is answered by
   writing the second adapter, not by designing harder. → here
