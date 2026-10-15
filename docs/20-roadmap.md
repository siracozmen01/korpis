# Build Order

**Status:** design **Date:** 2026-08-07 **Depends on:** every preceding document

---

## 1. This is order, not scope

Everything described in the other twenty-three documents ships. Nothing here removes anything; this
document decides only what is built before what, and why that particular sequence rather than a
more comfortable one.

The comfortable sequence is to build the visible parts first (a panel, a server list, a console)
and add the model underneath later. That sequence produced Pterodactyl's abandoned v2, and it is
worth being precise about why: **some decisions cannot be retrofitted, and building the visible
parts first is a commitment to all of them by default.**

---

## 2. Five ordering rules

**A. Anything that shapes stored history goes first.** `Intent`, `Plan`, and `Effect` determine
what the system can ever say about its own past. A system that mutated state directly for six
months cannot later produce an audit trail for those six months, and reconciliation cannot be added
to an imperative core without rewriting it, which is the abandoned-rewrite failure, exactly.

**B. Anything a third party depends on across an upgrade boundary goes early.** Once agents exist
on machines Korpis does not control, the protocol is frozen (§2 of `10-api.md`). Getting it wrong
before anyone has deployed it costs a week. Getting it wrong afterwards costs a major version and
everyone's trust.

**C. A security boundary is built before the thing it protects, never after.** Privilege separation
in the agent, kernel confinement, and grant evaluation are not hardening passes. The largest class
of Wings advisories, six of twenty-one, exists because the boundary was drawn after the feature
worked.

**E. A schema is frozen whole, even when it is implemented in parts.** Phase 0 freezes the API
schema (rule B), so every field a later phase needs must already be in it: the recursive tenancy
model, the grant with its delegation policy, the full Recipe and its config schema. A later phase
implements more of a schema; it never changes one. And an interface that later features plug into
is built before the first of them, so that nothing is written in-core and moved out afterwards: the
extension contract's core precedes the first backup target, identity provider, and step provider.

> Corrected 2026-10-12 (second external review, S57). The first version of this document froze the
> schema in Phase 0 while organizations, the Recipe, and the config schema arrived in Phases 2 and
> 4, and built S3 backup targets (Phase 1) and OIDC (Phase 2) before the extension contract they are
> providers of (Phase 4). Rule E is the rule both violated, and the phases below follow it.

**D. The second implementation of an interface is the test of the interface, and it must be the
most *different* one available.** Not the easiest second one. After the `oci` driver, the driver
interface is proven by `firecracker` (a separate kernel, no shared filesystem, no shared PID
namespace) not by `native`, which is similar enough to prove nothing. The same rule applies to the
second storage class, the second exposure mode, the second surface, and the second identity source.

Rule D sits alongside K-18 rather than against it: ship **one excellent** driver, and let the
second one's purpose be interface validation rather than coverage. Five adequate drivers is what
K-18 forbids; two excellent and maximally different ones is how the interface earns the right to be
called stable.

---

## 3. Dependency graph

An arrow means *built before*: the thing at the head needs the thing at the tail to exist.

```
  10-api: every schema, frozen whole (rules B and E)
  │   01-model's nouns; 08-identity's grant and recursive tenancy schema;
  │   09-recipes' Recipe and config schema
  │
  ├─→ 03-state ─→ 02-architecture ─→ 17-security ─→ 04-runtimes
  │   (Postgres,   (protocol, leases,  (privilege      (driver interface,
  │    I/P/E)       assignment,         separation,     `oci`, inside the
  │                 journal)            confinement)    boundary, rule C)
  ├─→ 08-identity (grant evaluation, capability tokens)
  └─→ 13-surface-cli (the reference client)                     Phase 0
        │
        ↓
  16-extensions, the contract core (process model, permissions, providers)
        │
        ├─→ 06-storage (backup targets are providers) ─┐
        ├─→ 05-scheduling        07-networking ────────┤
        ├─→ 02-architecture §3.1 (the relay) ─→ 14-streams
        └─→ 09-recipes (install DSL; step providers) ──┤
                                                       ↓
                                              11-surface-web    Phase 1
        │
        ↓
  08-identity in full (delegation, external identity as providers)
  15-observability (metering, tenant audit)                     Phase 2
        │
        ↓
  04-runtimes' second driver; 05-scheduling §8 (migration);
  07-networking §3.1 (edge failover groups, `stable`)           Phase 3
        │
        ↓
  16-extensions in full (actions, UI slots, delegated install, distribution);
  09-recipes' distribution and egg import; the chat client      Phase 4

  18-operations and 19-governance apply throughout, not after.
```

**This is build order, not reading order, and the two differ on purpose.** The `Depends on` line
at the top of each document records which documents a *reader* needs first. `03-state.md` depends
on `02-architecture.md`, because the store is argued from the protocol's needs; here the store is
built first, because the protocol writes to it. `16-extensions.md` depends on `09-recipes.md`,
because an extension is packaged as a recipe; here the contract core comes first, because the
install DSL's escape hatch is a provider. Neither is wrong; they answer different questions.

> Corrected 2026-10-12 (second external review, S70). The first version of this graph was presented
> without saying which kind of dependency it showed, and it disagreed with the document headers
> without explanation.

`10-api` is at the root because the schema is the only artifact every other component shares.
`08-identity` reaches everything because authorization is evaluated on every call in every document
above. `09-recipes` is split across three phases: its schema in Phase 0, because every intent names
a recipe and is shaped by its config schema, together with verification against the trust policy,
because every intent records what vouched for its recipe (§7 of `09-recipes.md`) and that field
cannot be added once intents exist; its install DSL in Phase 1, built on the contract core so that
its escape hatch is a provider from the start; and its distribution tooling, the signing tooling
among it, and egg import in Phase 4, with the ecosystem they serve.

---

## 4. Phases

### Phase 0: The parts that cannot be retrofitted

**Every schema, complete, and the protocol.** The full domain model of `01-model.md`, including
recursive organizations and quota sets; the grant schema with its delegation policy and conditions;
the full Recipe and its config schema, and the trust policy that admits a recipe; the agent
protocol with assignments, plans, and the node journal. Phase 0 implements only what its done-when
needs, but no later phase adds a field to a schema frozen here (rule E).

PostgreSQL with `Intent`/`Plan`/`Effect`, immutability enforced by the database. Grants,
attenuation, and capability tokens. The agent, dialling out, level-triggered, with full privilege
separation and lease fencing from the first commit. One runtime driver (`oci`), excellent, running
thin recipes. The CLI.

**No web client in Phase 0.** This will look wrong and it is the most important ordering decision
in the document. §1 of `13-surface-cli.md` makes the CLI the reference client precisely so that P2
is mechanically enforced; a web UI built while the API is still moving is how a project acquires a
privileged surface and, eventually, two disagreeing APIs. The CLI is also an order of magnitude
cheaper to build, so the model gets exercised sooner.

**Done when:** `korpis apply -f` places a workload on a node, converges it, survives an agent
restart without restarting the workload, and every action in that sequence is a row in the effect
log naming its grant, the denied ones naming instead their subject and the action that was
missing. `scripts/e2e-phase0.sh` checks exactly this, with the five binaries running as processes,
the recipe published with `oras`, and every step taken through the `korpis` CLI.

> Corrected 2026-10-13, by the script that checks it. This said that every action "including the
> denied ones" names its grant. A denial is the one case no grant authorized, so there is no grant
> to name; what §4 of `15-observability.md` records for it is its subject and the action that was
> missing, which is what an investigator needs, and that is what the script checks. The same run
> found the scheduler's bind and the lease it issues naming no grant at all, though both carry out
> an approved decision; they now name the grant of that decision (§3.3 of `01-model.md`).

That is the smallest artifact that proves the entire thesis.

### Phase 1: Usable by one operator on one machine

**The extension contract's core first**: extensions as workloads, permissions as grants declared
at install, event consumers, and the provider interfaces (§3 to §6 of `16-extensions.md`). Every
backup target, DNS provider, and step provider is a provider from the first one, so nothing is
written in-core and moved out later (rule E).

Then: the relay tier (§3.1 of `02-architecture.md`), which every console and file transfer runs
through. The web client. Durable streams and console. Volumes with kernel-enforced byte and inode
quota, snapshots, deletion, and content-defined-chunking backup with per-workload keys. The
shareable link of §6.1 of `08-identity.md`, the first test of Bet 1. The install DSL with its
built-in verbs. Scheduling: filter, score, bind, with `Explanation`; cordon; the shortlist
`FeasibilityQuery`, re-placement after a post-bind rejection, and the content locality and churn
signals (§2.1 and §2.2 of `05-scheduling.md`). Networking: `node_port` and `node_bound` exposure,
each node's reachability measured by the relay's probe (§9 of `07-networking.md`), and the default
egress policy of §6 there, with a workload's own policy applied by its node. The HTTP ingress on
one machine (§3.2 there): the node serves as its own edge for `ingress` endpoints, routing by name
to their workloads, with certificates the control plane obtains by ACME and renews. Every other
field of an endpoint declaration is refused at declaration until the phase that builds it, rather
than accepted and shown while nothing enforces it (Rule K-3): dedicated addresses in Phase 3,
shaping and accounting in Phase 2. Added 2026-10-15, when the egress default was built and the
declaration was found to accept what no node enforced; §3.2 of `07-networking.md` calls the ingress
table-stakes for the `service` shape, and without it the web app of this phase's own scenario could
not be reached by name. Rate limits, with the web client's login and link redemption that are the
first endpoints to need them: the two buckets and the unauthenticated limits of §10 of `10-api.md`,
and the recording of unauthenticated failures beside the denials Phase 0 already coalesces.
Workloads of lifecycle `task` and `scheduled`, with their runs and `RunPolicy` (§4 of
`05-scheduling.md`), after backups, so that a nightly backup is a scheduled run rather than a
command someone has to type; added 2026-10-14 by Finding 33 of `23-walkthroughs.md`, since no phase
had named them.

**Done when:** the single-machine constraint of §1 of `18-operations.md` is real, install, run
workloads, back them up, restore them, and read an honest console, with every displayed limit
kernel-enforced (K-3).


**Service discovery is named in Phase 1, not deferred to Phase 3.**

> External review, finding R19. Recorded in §16 of `23-walkthroughs.md`.

"A web app and its database" is a Phase 1 scenario, and one workload reaching another by a name
that survives the other's migration is a core requirement rather than a networking detail. The
*shape* of the answer touches the `Endpoint` model, the overlay, and the recipe schema at once, so
deciding it late means changing an API schema that Phase 0 froze.

What Phase 1 owes is the decision and the schema, which is small: whether dependencies are declared
on the intent or discovered through a name service, and what a name resolves to while its target is
between placements. What can wait for Phase 3 is the distributed implementation, because on one
machine the answer is a local resolver and the semantics are the same.

> Corrected 2026-10-13, during the Phase 0 spec sync. The decision was already made (§5 of
> `07-networking.md`: dependencies declared on the intent), so the schema was Phase 0's under rule
> E, and leaving it to Phase 1 would have been the very change to a frozen schema this paragraph
> warns against. `IntentBody.dependencies` and the node's measured `overlay` are now in the Phase 0
> schema. What Phase 1 owes is resolving a dependency and injecting its address; until then a
> declaration naming one is refused as not yet available, rather than stored as a promise nothing
> keeps. The overlay itself, which a dependency across nodes needs, is built in Phase 3 with the
> other machinery of workloads that span nodes; until then every node measures `unavailable`, and
> a workload and what it depends on are placed together.

### Phase 2: Multi-tenant, and the two bets that live there

Organizations and recursive delegation, and suspending one (§2.1 of `08-identity.md`).
`GrantTemplate`. `IdentityBinding` and role-to-template
mapping. Quota sets. Metering, with the exactness guarantees of §2 of `15-observability.md`.
Tenant-visible audit. External identity through OIDC, as a provider on the Phase 1 contract core.
Bandwidth shaping, a workload's reservation and limits (§5.2 of `05-scheduling.md`) and an
endpoint's own, and per-endpoint traffic accounting in kernel counters, which metering reads (§7 of
`07-networking.md`); until then a declaration that asks for either is refused, or, for a workload's
bandwidth, left unplaced on every node whose driver does not enforce it (Rule K-3).

**The chat client is not here.** It is a first-party extension and ships in Phase 4 (§10 of
`12-surface-discord.md`). What Phase 2 owes it is the machinery (external identity mapped to
subjects, roles mapped to grant templates) and that machinery is owed to OIDC, SAML, resellers, and
CI regardless, so no part of it is chat-specific work.

**Done when:** a reseller delegates to a sub-tenant who delegates to a customer, quota inherits
correctly, and an external role grants a person authority over one project without an account being
created anywhere.

### Phase 3: Isolation as a choice, and workloads that move

The `firecracker` driver. Rule D's validation of the driver interface. Migration, all seven phases,
resumable and reversible (K-4). Replicated storage classes. Edges in failover groups, which is what
`stable` exposure requires to exist at all and what lets it survive the move (§3.1 of
`07-networking.md`). The overlay (§9 of `07-networking.md`), so a dependency can cross nodes.
Dedicated addresses (§3 there), routed to whichever node a workload runs on.
Devices, including GPU. `AssignmentDelta` (§9 of `10-api.md`), once nodes hold enough workloads
for the difference to be measured. Drain and evacuate, as `Operation`s with their policy (§3 of
`05-scheduling.md`), since draining a node is migrating its workloads off it. With edges, the
count of a node's ports learns them: until then every `stable` endpoint reserves a port of its
node, which one an edge serves does not use (§3 of `07-networking.md`).

> Moved here from Phase 1 on 2026-10-15, when Phase 1's scheduling was built. Phase 1 named
> "cordon and drain", and §3.4 of `01-model.md` defines draining as migrating everything off a
> node, which this phase builds: a drain in Phase 1 could move nothing, so it would have been a
> cordon under another name, shown as `draining` while nothing drained. Phase 1 builds cordon,
> which keeps new placements off a node; the code refuses to set `draining` by hand, because a
> drain is an operation, not an operator's flag. On one machine, the installation Phase 1 is
> for, there is no other node to drain to. What it costs: until this phase, emptying a node for
> maintenance is cordoning it and stopping or deleting what runs there by hand.

**Done when:** a running workload with a 40 GB volume moves between nodes, keeps its public
address, and its meter neither resets nor double-counts across the cutover.

### Phase 4: The ecosystem

The extension contract in full: actions and commands, UI slots, installation inside a delegated
tree, and distribution (§4.1, §5, §8 of `16-extensions.md`), with the core extensions of §2 built
through it and not beside it. Recipes as OCI artifacts at ecosystem scale: publishing, signing
tooling, and egg import with its three grades.

The chat client lands here, as an extension, alongside the chat-adapter provider interface.

**Done when:** game query, RCON, a DNS provider, a Steam install step, and a complete chat client
are all extensions, and removing any of them removes only itself. The chat client is the sharpest
of these: an extension that is a *full* client, not a peripheral, is the strongest available proof
that the contract is sufficient, which is exactly why it is worth building through the contract
rather than in-core, where it would be easier and would prove nothing.

This is also the phase in which Korpis becomes a credible game panel, and that is deliberate
sequencing rather than an afterthought. `22-first-party.md` explains the position: general-purpose
underneath, unmistakably excellent at game hosting on day one, with every game-facing capability
delivered through the extension contract this phase exists to prove. The generality is structural
and costs nothing; being nobody's first choice would cost everything, and a first-party set is the
cheapest possible answer to it.

Two items in that set depend on Phase 3 rather than this one, the Minecraft handshake router and
hibernation both need the edge, which is why they arrive here and not earlier.

### Phase 5: The segments nobody else can reach

Windows drivers. Confidential-computing tiers if §13 of `17-security.md` resolves in their favour.
Protocol-aware routing. Everything the earlier phases' interfaces were shaped to permit.

---

## 5. Where each bet dies

The four bets in §4 of `00-overview.md` are falsifiable claims, so each has a phase at which it is
tested and a specific observation that would kill it.

| Bet | Falsified if | Tested in |
|---|---|---|
| **1: authority without accounts** | someone cannot be given real, bounded authority without a row in a users table, falsified early by the share link, later and harder by the chat extension | Phase 1 (share link), confirmed Phase 4 |
| **2: isolation is an operator choice** | the second, maximally different driver requires changing the driver interface | Phase 3 (partially observable in Phase 0) |
| **3: the diff is a first-class object** | Plan computation is too slow for interactive use, or common operations have to bypass it | Phase 1 |
| **4: correct tenancy gives reselling free** | any feature has to be written whose purpose is reselling | Phase 2 |

Bet 3 is testable earliest and is the one whose failure would be most expensive, since every other
document assumes Plans are cheap enough to compute on every change. If Phase 1 shows they are not,
that is the moment to find out, not Phase 4.

---

## 6. Risks, ordered

**1. Capability-based authorization has no precedent in this market.** §1 of `08-identity.md`
states this up front. Every competitor uses roles, every user expects roles, and the mitigation is
a presentation layer (`GrantTemplate` rendered as something role-shaped) rather than a schema
change. If that presentation does not land, the model is right and unusable, which is the same as
wrong. *Resolves: Phase 2.*

**2. Funding.** §9 of `19-governance.md` is honest that P10 removes every revenue mechanism the
incumbents use and that projects have died in this position. Structural mitigation: Korpis operates
no infrastructure, so its costs are near zero. *Never fully resolves.*

**3. Scope against one maintainer.** Twenty-four documents is a large model. The mitigation is Rules
A–D: the expensive-to-retrofit parts are small and early, and the large parts (drivers, storage
classes, exposure modes, surfaces, extensions) are additive behind interfaces that Phase 0 and
Phase 3 prove. *Continuously managed.*

**4. Plan latency.** Bet 3's failure mode. *Resolves: Phase 1.*

**5. The protocol freezing wrong.** Rule B puts the protocol first, which means it is designed with
the least implementation experience. The conformance suite and the agent-protocol support window (§4
of `10-api.md`) bound the damage; nothing eliminates it. *Resolves: Phase 3, when the second driver
and migration stress it.*

---

## 7. What building it requires

Not a large infrastructure, but not one machine the whole way either, and the point at which a
second and third are needed is specific.

**Phases 0–1: one machine.** Everything runs as VMs on it: PostgreSQL, the control plane, three or
four agents, a Firecracker or two.

| | Minimum | Comfortable |
|---|---|---|
| CPU | 8 physical cores | 12–16 |
| RAM | 32 GB | **64 GB** |
| Disk | 1 TB NVMe | 1 TB NVMe **plus a separate second disk** |
| Virtualization | VT-x/AMD-V with **nested virt** | none |
| Kernel | 6.1 LTS or newer | 6.6+ |

Three of those are load-bearing rather than preference:

- **Nested virtualization is not negotiable.** Bet 2 (isolation strength as an operator choice) is
  untestable without actually running Firecracker and QEMU, inside VMs. Most cheap VPS products do
  not offer it, which is why the primary machine is owned hardware rather than rented.
- **RAM is the binding constraint**, because microVM density and the hibernation claim of §6 of
  `22-first-party.md` are both memory questions.
- **A separate disk is genuinely needed.** §3 of `06-storage.md` is largely about `EDQUOT` arriving
  in the write path and about inode exhaustion, and verifying either means filling a filesystem on
  purpose.

The same machine is the CI runner. The conformance suites want VMs across a kernel matrix, which is
the second reason for nested virt.

**Phase 2 onward: two more machines, in two locations.** One box stops being enough at a specific
point, for specific reasons, migration across a real network rather than a virtual bridge, lease
fencing under a real partition, an edge with a real public address, and the nftables-versus-XDP
crossover of §11.1 of `07-networking.md`, which is a measurement that means nothing taken on a
virtual bridge. Modest machines suffice; no nested virt is needed, since tier work stays on the
primary. Plus one public IPv4, an IPv6 block, a domain, and any cheap S3-compatible bucket for
backup-target behaviour.

**Later and narrow:** one GPU, rented by the hour, for the device model of §6 of `05-scheduling.md`
in Phase 3. One Windows Server in Phase 5, where §13.5 of `17-security.md` observes that the
isolation table has no honest Windows column yet, that column gets written on that machine.

**Not needed, and worth saying because the reflex is to acquire them:** a Kubernetes cluster, a
managed database, separate CI infrastructure, or a load-generation fleet. The scale assumptions in
§9 of `03-state.md` are simulable on one box, and §1 of `18-operations.md` guarantees the
single-machine path stays real precisely so this stays true.

---

## 8. Not on the critical path

These can be built at any time by anyone and block nothing: additional recipes, themes,
translations, additional exposure modes, additional storage classes, additional identity providers,
SDKs in further languages, dashboards, and every extension in §2 of `16-extensions.md` beyond the
four that validate the contract.

That list is deliberately long. It is what Rule D buys: once an interface has been proven by a
second, maximally different implementation, the third through twentieth are contributions rather
than architecture.

---

## 9. Status

| Document | State |
|---|---|
| `docs/research/evidence.md` | complete: evidence, confidence-tagged, rules K-1…K-12, and the stated basis of K-13…K-18 |
| `docs/00-overview.md` … `docs/20-roadmap.md` | complete: the core twenty-one |
| `docs/21-stack.md` | complete: Rust throughout, TypeScript in the browser, and why |
| `docs/22-first-party.md` | complete: the recipe and extension set, and what it proves |
| `docs/23-walkthroughs.md` | complete: fifteen traces and their findings, and two external reviews, all patched or recorded |
| `README.md`, `CONTRIBUTING.md`, `SECURITY.md` | complete: entry point, review policy, disclosure |
| `LICENSE`, `LICENSE-APACHE`, `LICENSE-AGPL`, `LICENSE-CC-BY` | complete: the split, the full texts, and what it means for a host |
| succession document (§6 of `19-governance.md`) | **not written**; blocks the first release, not Phase 0 |
| `CLAUDE.md` | complete: the rules anyone editing these documents works under |
| `ARCHITECTURE.md`, `crates/` | Phase 0 built: thirteen crates, about 53,000 lines of Rust, 276 tests, twenty migrations, and the 41 calls of the public API |

Sixty-nine numbered open questions remain open, of 126 recorded. Ten of them carry a question
into the document that will settle it, so fifty-nine decisions are open, each assigned to the one
that will settle it. They are the design's honest edges, not omissions: an open question with an
owner is a decision deferred deliberately, and every one of them was recorded where the person who
hits it will find it.

**Phase 0 is built, and its done-when holds.** `scripts/e2e-phase0.sh` runs it as an operator
would: the five programs as processes, the recipe published with `oras`, and every step through
the `korpis` CLI. A workload is applied, placed, and converged; the agent restarts and the
workload keeps running in the same process; a reader's refused change is recorded with its
subject and the action that was missing; and every permitted action in the log names its grant.
Building it found the specification wrong or silent in more places than the reviews did, and each
is corrected in place where it was found, with the trace that crosses it noting the correction.
The one question owed during Phase 0, whether the protocol freeze waits for a 1.0 of the Rust
Connect implementation, is resolved in question 7 of `21-stack.md`: it does not.

**What Phase 0 does not do is homed.** Every refusal the code answers with "arrives in Phase N"
names a phase that names it in §4, and every part of a specified behaviour Phase 0 left unbuilt
says so where it is specified: the shortlist query and re-placement (§2.1 of
`05-scheduling.md`), the reachability probe (§9 of `07-networking.md`), rate limits (§10 of
`10-api.md`), and the assignment delta (§9 there). The succession document still blocks the first
release.

> Corrected 2026-10-13, as Phase 0 closed. This section said that nothing blocked the start of
> Phase 0 and that what remained was to build the model. Both were true when written, and
> neither describes a repository whose Phase 0 is built. The counts were also behind: the open
> questions had grown to 126 as building raised new ones, and a trace (§18 of
> `23-walkthroughs.md`) had been added to the fourteen.

The model is drawn, and its first phase is built. What remains is Phase 1.
