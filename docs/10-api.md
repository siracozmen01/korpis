# The API and Protocol Contract

**Status:** design **Date:** 2026-08-07 **Depends on:**
[`02-architecture.md`](./02-architecture.md), [`08-identity.md`](./08-identity.md) **Resolves:**
open question 3 of `02-architecture.md`; open question 5 of `03-state.md` **Implements:** Rules
K-7, K-8, Principle P2

---

## 1. One schema

Pterodactyl has two APIs (Application and Client) that overlap, disagree, and between them cannot
perform some operations at all. That is not a documentation failure; it is what happens when a web
UI is built first and an API is added beside it afterwards.

**Korpis has one schema. The web panel, the Discord client, the CLI, extensions, and third-party
integrations are all clients of it, and none can do anything another cannot** (P2).

The schema is the source of truth. The server, every client SDK, the OpenAPI document, and the
reference documentation are generated from it. "The API doesn't support that" and "the panel can do
something the API can't" are both structurally impossible; there is nowhere for the difference to
live.

---

## 2. Two surfaces, different obligations

| | Client API | Agent protocol |
|---|---|---|
| Consumers | web, CLI, chat, extensions, integrations | node agents |
| Reach | anyone | machines the operator runs |
| Stability | strong, with published deprecation windows | **effectively frozen** |
| Encoding | Connect. JSON and binary on one endpoint | binary, streaming |
| Authentication | grants and capability tokens (§6 of `08-identity.md`) | node key (§7 of `02-architecture.md`) |
| Evolution | additive; breaking changes take a new version | additive only |

They are separated because their obligations differ. A client API can deprecate something over
eighteen months and clients update. **The agent protocol is frozen for as long as agents exist on
machines Korpis does not control**, §1 of `02-architecture.md`. An operator with a node in a rack
they visit twice a year is a supported configuration, and that node's agent must keep working.

---

## 3. Encoding

**Protocol Buffers as the schema language, served over Connect.**

Protobuf because one definition generates the Go server, the TypeScript client, the Python and Rust
clients, the OpenAPI document, and the docs; because its compatibility rules are precise and
mechanically checkable; because streaming is native; and because the agent protocol carries a high
volume of observations and stream data where wire efficiency is real.

Connect rather than raw gRPC because it speaks Connect, gRPC, and gRPC-Web on one endpoint, works
through ordinary HTTP proxies and CDNs, and is reachable from a browser without a translating
proxy.

**The JSON/HTTP surface is first class, not a fallback.** This matters more than it appears. The
ecosystem Korpis needs (billing integrations, Discord bots, monitoring scripts, one-off automation)
is written by people who will reach for `curl` and `fetch`, and an ecosystem that requires a
protobuf toolchain to send one request is an ecosystem that does not form. So:

```bash
curl -H "Authorization: Bearer $TOKEN" \
     -H "Content-Type: application/json" \
     -d '{"workload_id":"wl_7f3a"}' \
     https://korpis.example/korpis.v1.WorkloadService/Restart
```

An OpenAPI 3.1 document is generated from the same protobuf, so the JSON surface is documented in
the tool everyone already uses.

---

## 4. Compatibility rules

Rule K-8 says governance is a day-one decision. These are its mechanical half, and they are
enforced by a schema-diff check in CI rather than by reviewer memory.

### Schema rules

| Never | Always |
|---|---|
| change a field number | reserve removed numbers, permanently |
| change a field's type | add new fields as optional, with a meaningful zero value |
| rename a field in the wire schema | tolerate unknown fields and unknown enum values |
| repurpose a reserved number | give every enum an `UNSPECIFIED = 0` |
| add a required field | treat absent as "unchanged" in updates, never as "clear" |

### Version rules

- The package is `korpis.v1`. A breaking change produces `korpis.v2`, and `v1` continues to be
  served for a published, dated period.
- **Agent protocol: the control plane supports every agent protocol version shipped by a release
  that is still supported, and never fewer than the current version and the two preceding.** A
  protocol version is not a release version: the protocol is versioned independently (§7 of
  `02-architecture.md`) and changes rarely and additively, so in practice the window spans many
  releases. The binding case is the LTS: a control plane on any supported release speaks to agents
  of every supported LTS for that LTS's whole life (§5 of `19-governance.md`), so an operator can
  move from one LTS to the next with the control plane first and the agents at leisure. Published
  as dates, not as vibes. Upgrade order is always control plane first, then agents, one at a time;
  a mixed fleet is a supported state.

> Second external review, finding S65. Recorded in §17 of `23-walkthroughs.md`. The window was "the
> current version and the two preceding", with "version" undefined. Read as release versions, an
> eight-week release train makes an LTS-to-LTS upgrade jump about six releases, which would put
> every agent left on the old LTS outside the window, against §4 of `18-operations.md` and §5 of
> `19-governance.md`. The window is now defined by what is supported, not by a count.
- Deprecation is announced, then warned about in response metadata, then removed after the
  published window. Nothing disappears without having warned in-band.

### Capability negotiation, never version sniffing

An agent's `Hello` carries its capabilities. A client reads `ServerInfo` for the server's. **No
component branches on a version number to decide what another component can do.**

This is the same rule as runtime drivers (§4.1 of `04-runtimes.md`) and it holds everywhere in
Korpis: two builds of the same version on different kernels have different capabilities, so a
version number is a fact about a build and never a statement about capability. Ask; do not infer.

---

**A field's default value never changes.**

> External review, finding R10. Recorded in §16 of `23-walkthroughs.md`.

Intent bodies are stored with defaults emitted (§3.1 of `03-state.md`), which freezes the meaning
of rows already written. This rule defends the same property from the other side, for every
consumer that did not read that section: a client generated against `v1` and a control plane
running a later version must agree on what an absent field meant, and they only do if the answer
never moved.

Changing a default is therefore a breaking change requiring a major version, and the cheaper move
is always available: add a new field with the new default and deprecate the old one on the schedule
§5 of `19-governance.md` already defines.

## 5. Actions

Grants name actions (§3 of `08-identity.md`), so the action vocabulary is part of the API contract
and carries a rule that is easy to get wrong:

> **Actions are never renamed and never split.** Adding an action is additive and safe. Splitting
> `workload.write` into `workload.config.write` and `workload.state.write` silently changes what
> every existing grant permits, either widening or narrowing authority that someone deliberately
> issued. That is an authorization bug shipped as a refactor.

New granularity arrives as **new, finer actions**, with the coarse action implying them. The coarse
one is deprecated for new grants and honoured forever for old ones. **A new capability that no
existing action covered is a new action implied by nothing**, so introducing it widens no grant
that already exists.

```
workload.read · workload.create · workload.write · workload.delete
workload.start · workload.stop · workload.restart    declare Power (§3.3 of 01-model.md)
workload.console.read · workload.console.write
workload.exec                                        a new process inside; implied by nothing
workload.file.read · workload.file.write
workload.secret.read                                 read a field the recipe marks secret
plan.read · plan.approve · plan.cancel
volume.read · volume.snapshot.create · volume.delete
backup.create · backup.browse · backup.read · backup.restore
stream.share
grant.read · grant.revoke
org.read · org.create · org.write · org.suspend · org.quota.write
project.read · project.create · project.write · project.delete
node.read · node.enroll · node.cordon · node.drain · node.evacuate
ext.<extension>.<action>              extensions define their own, namespaced
```

- **`workload.write` declares intents**: configuration, resources, runtime, endpoints. It is what
  `korpis apply` exercises (§7 of `13-surface-cli.md`). It does not imply power actions, so "may
  edit the config" and "may stop the server" stay separately delegable.
- **`workload.exec` is separate from `workload.console.write`.** Typing into a console the
  workload already exposes and starting a new process inside the workload are different powers,
  and the second is strictly stronger.
- **`plan.approve` is what an approver holds** (§3.6 of `08-identity.md`). Approval was central to
  the model and had no action a gateway could check when someone pressed Approve.
- **Delegating is not an action.** Whether a grant may be attenuated into children, how deep, and
  with which actions, is a property of the grant itself (§3.2 of `08-identity.md`), because it is
  checked against the chain rather than against a scope. `grant.revoke` remains an action because
  revoking within a scope is an authority over other people's grants, and `grant.read` because
  "who can restart this server" must be answerable by someone (§3.5 of `08-identity.md`).

> Second external review, findings S14 and S20. Recorded in §17 of `23-walkthroughs.md`. The first
> vocabulary had no action for approving a plan, declaring configuration, exec, or reading a secret,
> though other documents used `workload.write` and `console.write` as if it did. It also listed
> `grant.issue`, whose relation to attenuation no document defined. This correction is made before
> any release, which is the only time the rule above permits one; from the first release the
> vocabulary only grows.

The `ext.` namespace means an extension can define actions without colliding with core or with
another extension, and those actions attenuate and audit identically to core ones.

---

## 6. Mutations

Every mutating call carries two things:

```
idempotency_key   string    safe retry, a repeat returns the original outcome
expected_version  int       compare-and-swap against the object's current version
```

`expected_version` implements §3.2 of `03-state.md`: concurrent declarations are **detected, not
merged**. A mismatch returns `CONFLICT` carrying the intent that won, and the caller recomputes.
Last-write-wins is never the behaviour, because a Discord command and a web user editing the same
workload at the same moment is normal rather than exotic.

`idempotency_key` makes retry safe at the API boundary, matching §4.7 of `02-architecture.md` at
the agent boundary. A client that times out and retries never double-applies.

### Read-your-writes

> Resolves open question 5 of `03-state.md`.

Read replicas serve dashboards well and lag behind writes, which produces the worst possible
experience: an operator presses a button, the write succeeds, the page refreshes, and the change is
gone.

Every write response carries a `consistency_token`. A client passes it back on subsequent reads,
and the gateway routes those reads to a replica that has caught up, or to the primary if none has.
Clients that ignore the token get ordinary eventual consistency; the SDKs carry it automatically,
so correct behaviour is the default and nobody has to know why.

---

## 7. Streams

Console, logs, and events are server-streaming calls. Console input is client-streaming.

**Backpressure is explicit, and a gap is always visible.**

| Stream | Slow consumer |
|---|---|
| log / console output | bounded buffer; on overflow, drop and **emit a gap marker** with the byte range and duration lost |
| console input | blocks; input is never dropped or reordered |
| events | bounded; on overflow the subscription is terminated with `OVERFLOW` and must resubscribe from a recorded offset |

Silently dropping log lines is the standard behaviour and it is wrong under P4: a log with an
invisible hole in it is worse than no log, because someone will conclude from its absence that
nothing happened. Every gap is marked, sized, and timestamped.

Streams are durable and seekable (§3.6 of `01-model.md`, detail in `14-streams.md`), so a
reconnecting client resumes at an offset rather than restarting from an in-memory buffer.

---

## 8. Errors

Structured and machine-readable, never a message string clients parse:

```
Error
  code       INVALID | UNAUTHENTICATED | DENIED | NOT_FOUND | CONFLICT
             | UNSATISFIABLE | QUOTA_EXCEEDED | OVERFLOW | UNAVAILABLE | INTERNAL
  reason     stable identifier, e.g. "quota.memory.exceeded"
  field      path, for validation errors
  detail     structured, code-specific
  retryable  bool
  retry_after Duration?
```

### Existence is gated by read authority

```
subject can read the resource, lacks the action  →  DENIED
subject cannot read the resource                 →  NOT_FOUND
```

Returning `DENIED` for something the caller cannot see confirms that it exists. Across a tenancy
boundary that leaks another organization's workload names, which are frequently customer names.
Same rule as filtered `Explanation`s (§8 of `08-identity.md`): authority determines visibility
before it determines permission.

`DENIED` carries which action was missing, never which grant would have granted it, the second
would let someone map the authorization structure by probing.

---

## 9. Full resync is nearly free

> Resolves open question 3 of `02-architecture.md`.

The concern: §4.1 of `02-architecture.md` makes the protocol level-triggered, so the control plane
sends the **complete** set of intents assigned to a node. A node running several hundred workloads
re-receiving all of them on every reconnect is expensive, and reconnects are common.

**Resolution: the assignment set carries a digest.**

The control plane computes a digest over the assigned set, canonically encoded, sorted by workload
ID, covering each assignment's generation (§4.8 of `02-architecture.md`). On reconnect:

```
agent  → Hello { set_digest: sha256:4c1a… }
         ├── matches       → InSync                      zero bytes
         ├── known prior   → AssignmentDelta from that base   only what changed
         └── unknown       → AssignmentSet (full)             the safe path
```

The common case (an agent reconnecting after a brief network blip with nothing having changed)
costs one digest comparison and one small message. The control plane keeps a short history of
recent set digests per node, so an agent that missed a few changes gets a delta rather than the
whole set.

Level-triggered semantics are fully preserved: the agent is never *required* to have received any
earlier message, the unknown-digest path always works, and any inconsistency resolves to a full
sync. The digest is an optimization that can be discarded at any time without affecting
correctness, which is the property that makes it safe.

The digest is over the **set**, not over individual assignments. Assignments keep their generation
numbers as their identity; this is a set-level checksum. No second identity concept is introduced.

---

## 10. Rate limits

> Second external review, findings S15 and S28. Recorded in §17 of `23-walkthroughs.md`.

The first version applied limits **per grant**. That is evadable and unfair at once: attenuating a
grant mints a child with a fresh allowance, so anyone permitted to delegate could multiply their
own rate; and a grant held by a Discord role is one bucket for every member of the role, so one
member could exhaust it for all of them. It also said nothing about calls made before any grant
exists, which is where password guessing happens.

**Authenticated calls are charged to two buckets, and neither changes under attenuation:**

| Bucket | Keyed by | Why |
|---|---|---|
| the individual | the authenticated subject: the user, the token, the specific external user even when acting through a role, the specific bearer link | one person's or one credential's behaviour is bounded, and a role is not a shared allowance |
| the organization | the organization whose grant the request's chain descends from | minting children creates no new capacity, because every child descends from the same organization's grants |

IP addresses are never the identity of an authenticated caller: they are shared and spoofable, and
a limit that a legitimate tenant behind CGNAT trips because of a stranger is not a limit, it is an
outage.

**Unauthenticated endpoints are limited by what they target and where they come from.** Login,
second-factor verification, link redemption, and the chat-interaction exchange (§5.2 of
`08-identity.md`) are limited per source address and, separately, per targeted account or link,
with failures slowing that target exponentially rather than locking it, because a lockout is a
denial of service anyone can trigger against someone else.

**Denials are charged and coalesced.** A denied request costs the caller's buckets like any other,
so probing is not free. Every denial is still recorded (§4 of `15-observability.md`), but repeated
identical denials from one subject within a window are recorded as one effect with a count and the
window's bounds, so a client in a retry loop cannot turn the audit log into an unbounded write path.
Unauthenticated failures are recorded the same way, aggregated per source and target.

Expensive operations carry their own budgets, plan computation, migration, backup initiation, and
recipe fetches are not comparable to a status read and are not counted against the same allowance.
Limits appear in response metadata so a client can pace itself rather than discovering the ceiling
by hitting it.

---

## 11. Publication

Rule K-7 requires that a third party can write a conforming agent without reading Korpis' source.
That obligation is met by publishing, as a separate Apache-2.0 artifact (§5 of `00-overview.md`):

- the `.proto` definitions for both surfaces, and for the internal protocols between the agent's
  supervisor, privileged helper, filesystem workers, drivers, and relays (§6.2 of
  `02-architecture.md`), so that a third-party driver or agent component conforms to a published
  contract rather than to an implementation
- the generated OpenAPI 3.1 document
- the compatibility rules of §4 as a normative specification
- the **conformance suite**: the same idea as the driver conformance suite (§8 of
  `04-runtimes.md`): an executable definition of what conforming means, for agents and for clients

A protocol without a conformance suite is documentation. With one, it is a contract, and an
ecosystem can exist on top of it instead of a series of forks.

---

## 12. Open questions

1. **GraphQL.** A dashboard fetching a workload, its observation, its endpoints, and its recent
   effects makes four calls. GraphQL solves that and adds a second query surface with its own
   authorization story, its own cost-analysis problem, and its own way to disagree with the primary
   API. The likely answer is purpose-built aggregate endpoints instead, the benefit without the
   second surface. → here
2. **Webhook delivery guarantees.** Extensions and billing systems subscribe to events.
   At-least-once with a persistent outbox and consumer-side idempotency is the correct default;
   whether Korpis also offers ordered delivery per workload, and what that costs at scale, is open.
   → `16-extensions.md`
3. **Bulk mutations.** Setting a label on five hundred workloads through five hundred calls with
   five hundred `expected_version`s is unusable. A bulk form needs its own conflict semantics
   (all-or- nothing, or per-item results) and it interacts with `Operation` (§3 of
   `05-scheduling.md`), which already exists for exactly this shape of problem. →
   `05-scheduling.md`
4. **Long-poll versus streaming for low-frequency clients.** A Discord bot does not want a
   permanent connection per guild. Whether it subscribes to a stream, long-polls, or receives
   webhooks changes the operational profile substantially. → `12-surface-discord.md`
5. **Schema evolution for `Intent.body`. Resolved in §3.1 of `03-state.md`**: there is one
   representation, not two: the column holds the protobuf message in proto3 canonical JSON, keyed
   by field name, with `schema_version` recorded on the intent. The compatibility rules of §4 cover
   the database as well as the wire.
