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
  provides      []{type, name, arguments}  what it serves, `dns` / `cloudflare` (§5); a step's
                                        arguments, scalar types only
  actions       []ActionDecl            `ext.<name>.<action>`, Phase 4 (§5)
  ui_slots      []SlotDecl              Phase 4 (§5)
```

**How the block is written and checked** (2026-10-13, from the implementation). A need is
`{actions, scope, reason}`: `scope` is `target`, `organization`, or `self` (its own workload), and
`reason` is required, because it is the line the installer reads before approving. An action is
needed over a scope once. `egress` entries are `host:port`, an IPv6 host in brackets. A
subscription is `{kinds, min_severity, workloads}`, the filter of §5 of `15-observability.md`
without its projects: a document cannot know the projects of the organization it will be installed
into, so the installation narrows them. A document that subscribes must need `event.read`, since
delivery reaches only what that grant reaches, and a subscription without it would install cleanly
and deliver nothing, forever. `provides` entries are `{type, name}`, with a step's `arguments`
(§5), the type one of `dns`, `backup_target`, `step`, `health`, `console`, `identity`,
`edge_router`, `chat_adapter`, `key_management`, the name a lowercase word, and for `step`
`<publisher>/<step>`, the step part one or more lowercase words joined by dots
(`valve/steam.app`; corrected 2026-10-13, when the first step provider was built: the rule had left
the step part undefined, the validation read it as one word, and every example in §4 of
`09-recipes.md` and §4 of `22-first-party.md` failed it). The extension's name, its document's
`metadata.name`, is a lowercase word, because it names its actions (`ext.<name>.<action>`). A
document of kind `Recipe` with an `extension` block, or of kind `Extension` without one, does not
load. Everything else in the document is read as any recipe's, by the same parser, so a driver
preparing the extension's workload reads it unchanged.

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

**As built in Phase 1: the name is claimed at install, the stream at registration.** Installing an
extension records its `provides` entries, and an entry whose type and name another live
installation already serves in the same organization refuses the install with `provider.taken`;
so the uniqueness §4.1 states for step providers holds for every type, and it is decided once, in
the install's transaction, rather than racily between two streams. A registration is then one
open stream: its caller must authenticate as an installed extension (§9.5 of `08-identity.md`,
the `extension:<id>` subject), and its `Register` must name one of that installation's entries,
with a version and a concurrency of 1 to 1024 calls at once. Several streams of one installation,
its replicas, register beside each other. Each registration and each withdrawal is an effect,
`provider.register` and `provider.withdraw`, and so an event (§5 of `15-observability.md`).
Uninstalling deletes the entries and every registration with them, and the streams of withdrawn
registrations are closed.

The control plane runs as several replicas with no leader (§5 of `18-operations.md`), so a stream
is held by exactly one of them and every registration records which. The holding replica confirms
its registrations every five seconds; one not confirmed for thirty seconds belongs to a replica
that is gone, and whichever replica notices withdraws it. What it costs: when a replica dies,
its registrations stand for up to thirty seconds although no stream is behind them, and a call
routed to one in that window fails as retryable rather than reaching the provider.

**A call is queued in Postgres, because the replica that wants a provider called is rarely the
one holding its stream.** The caller writes the call with its deadline (at most five minutes) to
the organization whose installation serves it: the calling organization's own, or failing that
its nearest ancestor's, which is how a child organization's provider shadows its parent's. Each
replica claims, with `FOR UPDATE SKIP LOCKED`, the queued calls the streams it holds may take: only
a stream of the version registered most recently for that provider, and only as many as its
declared concurrency leaves free. One task per replica claims for all its streams, so no stream's
free slots are counted twice. It sends each call down its stream, and the provider's result, the
stream's end, or the deadline finishes it; at the deadline the provider is sent `CancelCall`, and
an answer after it is discarded. Queueing and finishing announce on one notification channel that
wakes the claiming replicas and the waiting callers, and both also re-read every second, so a
lost announcement makes a call late, never stuck. A call that finds no installation providing
what it names fails at once as `provider.unknown`, not retryable; one sent down a stream that then
ends, or whose registration is withdrawn or expires, fails at once as `provider.stream_ended`,
retryable; one nobody answered fails at its deadline as `provider.deadline`, retryable. A provider
whose stream has dropped is therefore unavailable only until it reconnects within the deadline:
a call queued meanwhile waits for it rather than failing.

"Newest" is the version registered most recently, not the highest by any ordering of version
strings, which a provider's own versions need not follow. This is the drain of §9 question 1: a
replacement registers beside the old version, new calls go only to it, the old one answers what it
already holds, and its stream is then closed. A rollback is the same operation, which is the
point. What it costs: a newer registration whose replica has died attracts every new call for up
to the thirty seconds before it expires, and those calls wait, then fail by their deadline if it
is shorter, even though the older version is still connected. Every call is one row written,
claimed, and finished, three transactions where an in-process call would need none; provider
calls are control-path operations (a record published, a backup started, a step run), not
per-request traffic, which is what makes that acceptable. Finished calls are kept an hour.

**The SDK.** An extension written in Rust links `korpis-extension`, which is Apache-2.0 like the
protocol (§2 of `19-governance.md`) and links nothing of Korpis but the wire contract, so an
extension carries whatever licence its author chooses. It fetches the credential from the identity
socket and renews it five minutes before it expires; it follows the subscriptions its installation
holds and acknowledges each event only once the handler returned, so what the handler failed on is
delivered again; and it serves a provider stream, answering `Ping` itself, running other calls
through the handler at most `concurrency` at a time, dropping a call at its `CancelCall`, and
reconnecting with a backoff of one second doubling to thirty whenever the stream ends, so a
control-plane replica going away costs a provider one reconnection. It stops only when it is
refused (its installation may not register that name, or is gone). An extension running inside a
container reaches the control plane through its node, which relays `/run/korpis/api.sock` to it
(§9.5 of `08-identity.md`); the SDK uses that socket wherever it finds it, and the network
otherwise, so the same extension runs in a container and, in a test, as a process on the host. The
egress exception this paragraph once waited for was replaced by the relay (Finding 57 of
`23-walkthroughs.md`). Verified 2026-10-15 by a test extension the SDK builds, run in a container
on a real node against a control plane listening on loopback: it registers its provider, answers a
ping, and hears an event, every call through its node's relay.

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

**How an install is decided** (2026-10-13, from the implementation). A dry run resolves the
artifact, checks where it goes and who may install it, and answers with the document's manifest
and the grants it would issue, so the installer reads the needs before approving them. The real
install then requires `approved_needs` to equal the document's needs exactly, in their order,
with their reasons: an approval is of the list the installer read, never of a list rebuilt from
it. A need over `target` is granted on the project when the extension is installed into one, on
the organization otherwise; over `organization`, on the organization; over `self`, on the
extension's own workload, which the same transaction creates. Each grant's parent is the first,
in identifier order, of the installer's grants that the attenuation check of §3.2 of
`08-identity.md` accepts, so an installer whose grants may not be delegated can install nothing
that needs anything, whatever actions those grants carry. The workload, its plan, every grant,
the installation record, and their effects (`grant.issued` for each grant, `extension.install`
under the installer's grant) commit together. An installation's status is its workload's, as
observed: `running`, `failed` when it crashed or its node cannot run it, `stopped` otherwise;
`incompatible` arrives with §6.

**How an uninstall is decided** (2026-10-13, from the implementation). Uninstalling needs
`extension.uninstall` where the extension is installed and `workload.delete` on its workload,
since it proposes that workload's retirement. One transaction revokes every grant the
installation holds (each with its `grant.revoke` effect), records when it was uninstalled, and
proposes the retirement, which waits for a human like every retirement (§3.3 of `01-model.md`).
The extension can do nothing meanwhile: its workload still runs until the retirement is approved,
but it holds no grant, and a credential its node obtains for it now authenticates as
`workload:<id>`. The installation's record stays, because effects name its subject for as long
as the audit record is kept; its name is free again where it was installed, while its workload's
name stays taken until the workload is retired.

**An extension's artifact is installed, never declared.** A declaration through the workload API
that names an extension's artifact is refused, and so is one that gives an extension's workload
any artifact but the digest it was installed from, or upgraded to through the extension service
(§9 question 6) (§3.5 and §3.7 of `01-model.md`, §8 of `09-recipes.md`).
Without the first rule the same code would run as a plain workload, holding none of its grants and
authenticating as `workload:<id>`, which fails in ways nobody can diagnose; without the second, a
new version could arrive with needs nobody approved. Everything else about the workload (its
configuration, its power, its placement) is declared as for any workload. Moving an installation
to another version is an operation of its own (§9, question 6).

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

**As built in Phase 1** (2026-10-15). The pin is part of the intent: its recipe reference carries,
for each step kind the install names, the installation that answers it and that installation's
artifact digest, set by the control plane at declaration whatever a client sends. Declaration
walks up from the declaring organization to the nearest one with a live installation providing
the kind, checks every step's arguments against the schema that installation's manifest declares,
and refuses the declaration, on `body.recipe` naming the step, if no installation provides the
kind or the arguments do not fit. A pin that differs from the previous intent's is an install
change, as a new recipe digest is. What it costs: upgrading a step provider's installation changes
its digest, so every workload pinned to the old one has to be declared again, which pins the new
one and plans a reinstall, before its install can resolve; until then its install fails, saying
so. That is the lockfile's promise kept, not a defect of it: an upgrade that silently changed what
every workload installs is the thing a pin exists to stop.

**As built in Phase 1.** Zones, backup repositories, and edges are not yet objects with owners
of their own, so the resource a provider serves belongs to the organization its registration
belongs to: installing anything that provides needs `extension.install` over the organization as
well as where it is installed, and an identity provider is installed into the organization
itself, never one of its projects. When those resources become objects with owners (backup
repositories, §5.3 of `06-storage.md`; the edge, §3.1 of `07-networking.md`; DNS zones, which no
document models yet), the check narrows to their owner; it never widens past the organization.

> Noted 2026-10-19, as Phase 1 closed. Backup repositories became objects with owners in Phase 1,
> an organization or one of its projects (§5.3 of `06-storage.md`), but a repository names its
> backup target by the provider's name in its organization rather than being what the provider
> serves: the target holds every repository of the organization that names it. So the check
> stays at the organization for a backup target too, and the paragraph above is right in what it
> requires while wrong to list repositories among the objects that do not exist yet.

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
slow or dead consumer reads the log at its own pace, so its backlog never grows inside the
control plane, and a position it let fall out of retention is answered with a visible gap marker
(§5 of `15-observability.md`).

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
implement: publish DNS records (§3.5 of `07-networking.md`), run a health probe, install a Steam
application, keep backups in an object store (§5.6 of `06-storage.md`), verify an identity
assertion, route a protocol.

Calling out is the direction that carries risk, so the contract is strict:

- Every provider call has a deadline. There is no unbounded wait on third-party code.
- Failure behaviour is **declared per provider type**, never guessed. A DNS provider that is
  unreachable does not block a workload from starting; the name it was to publish is `failed` in
  the workload's status with the provider and the reason attached, and reconciliation retries (§3.5
  of `07-networking.md`). (Corrected 2026-10-16: this said the endpoint's observation became
  `unsatisfiable`, but an observation is what a node reports.) An identity provider that is
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

  **As built in Phase 1.** `RunStep` carries the step kind, its arguments, the workload, and the
  node's platform; the answer lists at most a thousand artifacts, each a relative path, an `http`
  or `https` URL, its SHA-256, its size and mode, and whether it is an archive to unpack under the
  `extract` verb's rules, with `pinned` and what it `resolved` to. An answer whose path climbs out
  of where it is written or whose content has no hash is recorded as `provider.malformed` and
  never reaches a node, and the breaker counts it. The argument schema is the `arguments` of the
  step's `provides` entry, in the form an action declares its own, restricted to `string`, `int`,
  and `bool`: a step runs under the extension's grant, not the recipe author's, so a resource
  argument would hand the author's choice of object to authority the author does not hold. A
  recipe's step is checked against it before any provider is called. The idempotence declaration
  is corrected in place: see §4 of `09-recipes.md`. Failure reasons are the provider's own, in
  the answer, as for every other type.

  > Corrected 2026-10-15, Finding 50 of `23-walkthroughs.md`. Each artifact's path was "beneath the
  > volume root", and a workload with two volumes has two roots: nothing said which, and neither
  > the provider nor the node could tell. The recipe's step names it: its `to`, a workload path in
  > one of the volumes, which the provider is never asked and may not declare as an argument, and
  > each artifact's path is relative to it (§4 of `09-recipes.md`).

  **Who calls it, as built** (2026-10-15). The node running the install asks, over its session,
  for one provider action of an install step (`StepRequest`: plan, step, action, platform), and
  the control plane answers on the same stream (`StepResolution`). A provider has no stream to a
  node and a node has none to a provider, so the control plane is the only component that holds
  both the node's question and the provider's registration. It answers only the node the workload
  is placed on, only for an install step of a plan still open, and only through the installation
  the intent pinned at its digest (§4.1); anything else is refused, `step.not_assigned`,
  `step.not_a_step`, or `step.provider_changed`, and none of those is retryable. The provider's
  own failures reach the node with the provider's reason and retryability. An unpinned result
  sets the workload's `unreproducible` mark (§3.3 of `01-model.md`) with what it resolved to,
  before the answer is sent. What it does not do: it checks the pin and then calls, so an upgrade
  landing between the two is answered by the upgraded installation, once; the next resolution
  refuses.

  On the node, an install with provider steps asks for each and starts its worker only once all
  are answered; a refusal fails the install step with the reason. The answers are written beside
  the reconciler's progress before the worker starts, so a worker lost and resumed at an action,
  or an install retried after a failure, runs the same actions, whatever a channel holds by then;
  they are dropped when the install succeeds or another install of the workload replaces it. Each
  file artifact becomes a `fetch` to its path beneath `to` and a `chmod` to its mode, and each
  archive is fetched into the node's content store and unpacked there by an `extract` that reads
  only what fetches downloaded (§6 of `06-storage.md`). The node checks the answer again before
  using it: the control plane checked it, and the node does not take that on trust.

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

> Corrected 2026-10-13, while building the breaker. The second point above says failure behaviour
> is declared per provider type, and this section declared it for two of the eight types (DNS and
> identity); the other six were exactly the guess it forbids. They are declared below.

**As built in Phase 1: what each type's failure means.** The declaration lives in the domain
(`ProviderType::failure_policy`), so every call site reads it rather than deciding afresh:

| Type | Its dependents, when it fails |
|---|---|
| `dns`, `backup_target`, `edge_router` | observed `unsatisfiable`, the provider named in the reason; reconciliation retries |
| `identity`, `key_management` | fail closed: what it would have asserted, or the key it would have opened, is refused; nothing else stands in |
| `step` | the install step fails with its reason, and the plan stops at that step |
| `health` | the health it judges is `unknown`, never `unhealthy` |
| `console`, `chat_adapter` | the request that needed it fails as unavailable and may be retried |

A health provider that is down reports nothing about the workloads it probes, and treating that as
`unhealthy` would let a failed prober get every workload it probes restarted; so it is `unknown`,
the observation's own value for health nobody has judged (§3.3 of `01-model.md`). A console or chat
adapter serves one request at a time and nothing else depends on it, so its failure is that
request's alone.

**As built in Phase 1: the breaker.** Each provider, by the organization whose installation
serves it, its type, and its name, has one breaker, kept in Postgres because any replica may call
any provider. What evaluates it is the caller of each call, after the call finishes, in a
transaction holding the breaker's row lock, and the information it judges by is the queue of §3.3
itself: the provider's last twenty calls finished within five minutes, so the evidence is never a
second copy that could disagree. A call counts as failed if it failed retryably (its deadline
passed, its stream ended, or the provider said retrying may help); a provider's answer that the
request itself is wrong is a correct answer. With at least ten calls judged, the breaker trips
when half of them failed, or when the median call took four fifths of its deadline or more, the
time spent queued for a free slot included. Tripping opens it for ten seconds; while open, a call
fails at once with `provider.circuit_open`, retryable, carrying when to retry, and is never
queued. After the backoff the next call is the probe, and the others are refused until it
finishes: a probe that neither failed nor took four fifths of its deadline closes the breaker,
anything else reopens it for twice as long, up to five minutes. A probe whose caller died is
replaced once its own deadline has passed. Tripping writes `provider.degraded` with what was
measured and how many calls are queued behind it, and closing writes `provider.recovered` (§5 of
`15-observability.md`). Operators read every provider's breaker and measurements through
`ProviderService.ListProviders`, so latency is published, not merely logged.

What it costs, and what it does not do. Each call takes two transactions more than §3.3's
three, one to be admitted and one to be judged, and every caller of one provider takes that
provider's row lock briefly. A provider called fewer than ten times in five minutes is never
tripped: its failures reach each caller as they happen, bounded by their deadlines, but no pattern
is drawn from so few. The judgement is made only when a call finishes, so a provider nobody calls
stays as last judged, open or closed, until somebody does; an open breaker over a provider nobody
calls costs nothing. Thresholds are fixed constants, not configuration, until a deployment shows
a reason for another value.

**A key management provider never answers with a key in the clear**, as built 2026-10-14. It
holds a repository's key-encryption key for its tenant (§5.5 of `06-storage.md`): it makes a
workload's data key and wraps it, and opens a wrapped one, for one context,
`<repository>/<workload>`. Every call names a recipient's one-time X25519 public key, and the
answer is the data key sealed to it with HPKE (RFC 9180, base mode, DHKEM(X25519, HKDF-SHA256),
HKDF-SHA256, ChaCha20-Poly1305), the context bound in. Answers travel the stored queue of §3.3 like
any other, so the rule is what keeps a data key out of the database, its write-ahead log, and its
backups: what is stored opens only for the recipient, whose secret never leaves the process that
made it. The SDK seals; an implementation returns the key and never handles a recipient.

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

**As built in Phase 1.** The contract version is `korpis.extension.v1`, and a capability names an
interface of the contract the control plane actually speaks, never one it merely tolerates:
`events.subscribe` (subscriptions and their delivery, §5 of `15-observability.md`),
`identity.credential` (the credential socket, §9.5 of `08-identity.md`), `provider.serve` (a
provider's stream, §3.3: registering, being pinged, being cancelled), and the calls of five
provider types: `provider.backup_target` (§5.6 of `06-storage.md`), `provider.dns` (§3.5 of
`07-networking.md`), `provider.identity` (re-verification and assertion, §5.1 and §5.2 of
`08-identity.md`, added in Phase 2), `provider.key_management` (§5.5 of `06-storage.md`), and
`provider.step` (§4 of `09-recipes.md`). The `provider.<type>` of health, console, edge router,
and chat adapter providers is added when that type's own calls join
the contract; until then an extension requiring one is refused, because installing it would leave
it waiting for calls nothing makes, the failure six weeks later this section forbids. A capability
is offered once its calls are in the contract and the SDK, even while the control plane's own use
of them is still being built (DNS records are published for endpoints, which Phase 1 creates
later), since the provider is then correct to wait. The check runs at install, a dry run included,
so the installer reads the refusal before approving anything: an unknown contract version, or every
required capability that is missing, at once, is refused as `extension.incompatible` naming what is
missing and what is offered. A client reads both lists from `ServerInfo` (`extension_contracts` and
`capabilities`), which is how an extension's SDK negotiates rather than compares version numbers.

**Signing in, designed 2026-10-20** (§9.7 of `08-identity.md`). `provider.identity` grows, as the
rules above allow, by addition alone. An identity provider that can sign people in says so when it
registers (`sign_in`), and answers `Authorize {state, nonce, code_challenge, redirect_uri}` with
the address to send a person to; one that registered without it is never asked, so a Discord
provider is unchanged. Its `Assert` evidence may be an authorization code, with the verifier, the
redirect URI, and the nonce as headers. And its answer to `VerifyMembers` may name members it
cannot tell about (`unknown`), beside those present, so that a provider holding no current evidence
of someone, an OIDC provider without their refresh token or any SAML provider, keeps their last
confirmation standing rather than ending it. A provider built before this answers no `unknown`, and
every member it does not name present is ended as before. Built 2026-10-20 in the contract, the SDK
(`IdentityProvider::authorize`, refused by default, and `Registration::sign_in`), and the control
plane's re-verification, which keeps an `unknown` member's record as it was and holds a person's
own binding unknown to the provider as if it could not be asked.
The control plane keeps `sign_in` with the registration, refuses it from a provider of any other
type, and shows it in `ListProviders`. A sign-in's calls carry secrets the stored queue would
otherwise keep for the hour: the state and nonce of an `Authorize`, the code and verifier of an
`Assert`. The control plane empties them from the stored call as soon as it has the answer, and
the sweep empties those of a call whose caller died first within a minute, as it does an access
answer's secret (§5.6 of `06-storage.md`).

---

## 7. What extensions cannot do

Stated as a list because the boundary is the design:

- read or write the database
- modify, replace, or read Korpis' own files
- hold authority exceeding their installer's
- receive another subject's capability token (acting for an asserted user goes through a ticket,
  §5.2 of `08-identity.md`: one action on one object, once, redeemed only beside the extension's
  own credential, so the extension holds no authority of the user's)
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
breaker policy. It is a drain, applied to one registration rather than one node. As built, "new"
means registered most recently, and the routing is the claim rule of §3.3.
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
6. **Upgrading an installation. Resolved here.** Added 2026-10-13, when §4 made an extension's
   workload run only the digest it was installed from. Question 1 holds, since an upgrade is a
   rolling update of the workload, but the rolling update must come from the extension service and
   not from a plain declaration: the new version is resolved under the trust policy, its needs are
   compared with the approved ones, a need that grew is approved again by someone whose grants
   cover it, grants are issued or revoked for the difference, and only then is the new digest
   declared on the same workload, keeping the subject `extension:<id>` and everything recorded
   against it. It is built with the versioning of §6, where a contract version that changed is
   checked the same way.

As built, `ExtensionService.UpgradeExtension` takes the installation and a tag or digest of the
artifact it was installed from; another extension's artifact, or the digest installed already, is
refused. The upgrader needs what installing needed (`extension.install` where it is installed,
and over the organization when it provides anything), the new version must pass §6's check, and
the approved needs must be exactly the new document's, read from a dry run. Each grant the
installation holds whose actions and object a new need still asks for is kept, so a need that did
not change is not re-approved in substance and keeps its place in the grant tree; a need no held
grant serves is granted from the upgrader's own grants, as at install; a grant no need asks for is
revoked, with everything descended from it. `provides` entries the new version keeps keep their
registrations, so the old version's streams go on serving until its replicas are replaced, the
drain of question 1; dropped entries end with their registrations, and their calls in flight fail
as retryable. Subscriptions whose filter is unchanged keep their position; new ones start at the
end of the log. The new digest is declared on the same workload with the configuration it runs
with now, and the plan that declaration produces is approved like any other. All of it is one
transaction, which refuses with `extension.changed` if the installation changed since it was read.
What it does not do: change the configuration, which the new document's schema must still accept
as it stands, or the subscription filters an installer narrowed, which are the document's.
