# Architecture of the code

This file describes how the implementation in `crates/` is organized and the rules that keep it
that way. The specification of what Korpis does is in [`docs/`](./docs/00-overview.md); this file
is about where the code that does it lives. The rules for writing that code are in
[`CONTRIBUTING.md`](./CONTRIBUTING.md#code-rules), and `scripts/check.sh` enforces the ones a
machine can check.

## 1. The shape: ports and adapters, with the domain in the middle

The control plane is built as a hexagon (ports and adapters). There are three layers, and
dependencies point inward only:

```
            driving adapters                       driven adapters
   (something calls into Korpis)            (Korpis calls out to something)

   korpis-server  (Connect API)              korpis-store       (PostgreSQL)
   korpis-cli     (operator)        ----->   korpis-driver-oci  (containerd)
           |                       ports            ^
           v                    (traits in          |
   +------------------------+  korpis-app)   implements
   | korpis-app             |-------------------------+
   |   use cases, ports     |
   |  +------------------+  |
   |  | korpis-core      |  |
   |  |   the domain     |  |
   |  +------------------+  |
   +------------------------+
```

**Domain (`korpis-core`).** The nouns of `01-model.md` and the rules that hold between them:
identifiers, actions, scopes, grants and their algebra, the effect record, and later workloads,
plans and placements. It is pure. It does no I/O, has no async runtime, knows nothing of
PostgreSQL or the wire schema, and reads the time only when a caller passes it in. Everything in
it can be tested with plain values, and the same code runs in the control plane, the agent, and
the relay (§10 of `08-identity.md` depends on that).

**Application (`korpis-app`).** One use case per thing a client can ask for: create an
organization, issue a grant, declare an intent. A use case authenticates nothing and parses
nothing; it receives a caller and domain values, authorizes through the one authorization path
(P6), performs the change, and records the effect in the same transaction as the change (§4 of
`03-state.md`). What a use case needs from the outside world is declared here as a **port**: a
trait such as `GrantStore` or `Clock`. The application layer is async but runtime-agnostic: it
uses `Future`s, never `tokio`, and it never names `sqlx`, `connectrpc`, or a generated wire type.

**Adapters.** Everything that touches a socket, a file, a database, or a process. A *driven*
adapter implements a port (`korpis-store` implements the store ports on PostgreSQL). A *driving*
adapter turns an outside request into a use-case call and its result back into a response
(`korpis-server` translates Connect requests; it holds no business rule). The binary that wires
concrete adapters into the use cases is the **composition root**, and it is the only place that
knows every layer.

### Why this shape, and what it costs

The decisions Korpis cares most about are authorization, effect recording, and reconciliation, and
each has to be the same no matter which door a request came through: the API today, the agent
stream and the CLI next, the relay later. Putting them in a layer that cannot see a door is how
that is guaranteed rather than hoped for. The same separation lets the domain be shared with the
agent and relay without dragging a database driver along, and lets the PostgreSQL adapter be
tested against the real database while the rules are tested without one.

The cost is translation. A request crosses three type systems on its way in (wire, domain,
storage) and each boundary has a mapping module that has to be written and kept honest. That is
real code and real review load, and it is accepted because the alternative, letting wire or row
types leak into the rules, is how the rules end up depending on transport details. The mappings
live in the adapters, are tested there, and are the only code that knows both sides.

## 2. Crates

| Crate | Layer | Licence | What it holds |
|---|---|---|---|
| `korpis-proto` | wire contract | Apache-2.0 | the protobuf schema and generated types |
| `korpis-wire` | wire mapping | AGPL-3.0 | domain to and from `korpis.v1`; the stored body |
| `korpis-core` | domain | AGPL-3.0 | principals, authority, tenancy, workloads, plans, recipes |
| `korpis-app` | application | AGPL-3.0 | use cases and the ports they need |
| `korpis-store` | driven adapter | AGPL-3.0 | PostgreSQL: migrations and the store ports |
| `korpis-recipe` | driven adapter | AGPL-3.0 | the recipe document format and OCI registries |
| `korpis-server` | driving adapter, root | AGPL-3.0 | Connect API, TLS, keys, `install`, `serve` |
| `korpis-agent` | its own hexagon | AGPL-3.0 | the node supervisor (§4 of `02-architecture.md`) |
| `korpis-helper` | its own hexagon | AGPL-3.0 | the privileged helper, synchronous, minimal |
| `korpis-driver-oci` | driven adapter | AGPL-3.0 | the `oci` runtime driver over containerd |
| `korpis-containerd` | third-party wire contract | Apache-2.0 | containerd's API, vendored and generated (§6 of `21-stack.md`) |
| `korpis-extension` | client SDK | Apache-2.0 | what an extension is written with: identity, events, provider stream |
| `korpis-tls` | driving adapter, shared | AGPL-3.0 | trust bootstrap: the installation CA pinned by fingerprint, then trusted alone, for the agent and a relay |
| `korpis-stream` | driven adapter, shared | AGPL-3.0 | streams on disk: records, segment files, redaction, rate limits, read by the agent; and `korpis-stream-writer`, the binary a containerd shim starts beside each workload to write its stream (`14-streams.md`) |
| `korpis-cli` | driving adapter | AGPL-3.0 | the `korpis` command |
| `korpis-e2e` | tests only | AGPL-3.0 | a real node end to end: control plane, agent, driver, helper |

The agent and the helper are separate programs with their own use cases; they share the domain
crate and the wire contract and nothing else. The helper does not link the authority module of the
domain (the `authz` feature is off for it), because its attack surface is the reason it exists
(§6.1 of `02-architecture.md`). Nor does it link an RPC stack or an async runtime: the wire crate's
generated services are its `services` feature, which the helper leaves off, so it takes the
messages alone, and `scripts/check-layers.sh` fails if tokio, hyper, or connectrpc ever reaches
its dependency graph.

`korpis-wire` is the one translation between the domain and `korpis.v1`, shared by every program
that speaks the schema: the API adapter today, the CLI and the agent next. A translation that
refuses its input names the field and returns `WireError`; each adapter turns that into its own
error (the API's is `request.invalid`, §8 of `10-api.md`). It also holds the encoding of the
intent body that §3.1 of `03-state.md` stores in the database.

`korpis-recipe` is the one place the recipe's YAML form and its OCI artifact layout are read and
written (§2 of `09-recipes.md`). The server uses it as the `RecipeSource` port's adapter, and the
CLI will use the same crate to lint and push a recipe, so an author's tool and the control plane
cannot disagree about what a valid recipe is.

## 3. The dependency rule, enforced

| Crate | May depend on (of ours) | Must never depend on |
|---|---|---|
| `korpis-core` | nothing | `tokio`, `sqlx`, `connectrpc`, `korpis-proto`, any I/O crate |
| `korpis-app` | `korpis-core` | `tokio`, `sqlx`, `connectrpc`, `korpis-proto` |
| `korpis-wire` | `korpis-core`, `korpis-proto` | `korpis-app`, `korpis-store`, `sqlx` |
| `korpis-store` | `korpis-core`, `korpis-app` | `connectrpc`, `korpis-proto` |
| `korpis-recipe` | `korpis-core`, `korpis-app` | `sqlx`, `connectrpc`, `korpis-proto` |
| `korpis-extension` | `korpis-proto` | every AGPL crate of ours, and `sqlx` |
| `korpis-tls` | nothing | every other crate of ours, `connectrpc`, `tokio`, `sqlx` |
| `korpis-stream` | nothing | every other crate of ours, `connectrpc`, `tokio`, `hyper`, `sqlx` |
| `korpis-server` | all of the above | |

`scripts/check-layers.sh` reads the dependency graph with `cargo tree` and fails the build if any
row of that table is broken. The compiler already makes an inward dependency impossible to forget;
the script makes an outward one impossible to add quietly.

## 4. Bounded contexts

Within each layer, code is grouped by bounded context, following the document that owns the
decision (the table in `CLAUDE.md`). A context has the same module name in every layer, so the
domain rules, the use cases, the storage, and the API mapping for one subject share one name:

| Context | Owning document | Module name |
|---|---|---|
| tenancy: organizations, projects, quota sets | `01-model.md`, `08-identity.md` | `tenancy` |
| authority: grants, delegation, tokens, credentials | `08-identity.md` | `authz` |
| audit: the effect log | `03-state.md`, `15-observability.md` | `audit` |
| workloads: intent, plan, power | `01-model.md`, `03-state.md` | `workload` |
| recipes: references, configuration schema | `09-recipes.md` | `recipe` |
| placement: nodes, scheduling, leases | `02-architecture.md`, `05-scheduling.md` | `placement` |

Contexts talk to each other through the domain types and through use cases, never by reaching into
another context's storage.

## 5. Inside a crate

Files are named for what they hold, and a module with children is `name.rs` beside a `name/`
directory, never `name/mod.rs`. No file exceeds 400 lines, tests included; a file approaching
that is a module asking to be split by responsibility, not by line count. Unit tests sit in a
`#[cfg(test)] mod tests` at the foot of the file they test, or in `name/tests.rs` when they would
crowd it. Tests that cross a crate boundary or need a database live in the crate's `tests/`
directory, one file per behaviour.

## 6. Where to put a new thing

- A rule that would be true whatever the transport and storage: `korpis-core`.
- Something a client or a node can ask the system to do: a use case in `korpis-app`, in the
  context's module, with the effect it records.
- A new kind of question for the database: a method on the context's port in `korpis-app`, and its
  implementation in `korpis-store`.
- A new RPC: its schema in `korpis-proto`, its mapping and handler in `korpis-server`, both calling
  an existing or new use case. A handler with an `if` that is not about the wire is in the wrong
  crate.

## 7. What crosses a boundary, and in what form

**Requests.** A driving adapter turns wire messages into domain values before a use case sees
them, and refuses what does not parse: a malformed identifier, a name with a control character,
a label outside the syntax of `korpis_core::labels`. A use case therefore never receives a string
where a domain type exists.

**Stored data.** The store owns the format of everything it writes. Structured columns hold
documents defined in `korpis-store/src/stored/`, not serialized wire messages or domain structs,
so a field renamed in the protocol or a struct reshaped in the domain changes a conversion, never
data already written. That matters most for the effect log, which is append-only and can never be
rewritten. Principals are stored as their canonical key (`korpis_core::principal`), which parses
back to exactly one principal.

**The one exception: the intent body.** §3.1 of `03-state.md` decides that `intent.body` holds the
`korpis.v1.IntentBody` message in canonical JSON with defaults emitted, so that the schema and its
compatibility rules cover the database as well as the wire. The store cannot depend on the wire
schema (§3), so the format reaches it as a port: `IntentCodec` in `korpis-app`, implemented over
`korpis-wire` by the composition root and handed to `PgStore::connect`. The store writes and reads
the column through it and knows nothing of protobuf. The cost is a seam that has to be wired: a
store built with another codec would write another format, which is why the test databases of
`korpis_store::testing` are built with the same encoding the server uses.

**Idempotent replays.** §6 of `10-api.md` promises that a retried mutation returns the original
response. The guarantee belongs to the transport, but its atomicity belongs to the use case: the
response must be recorded in the transaction of the change. So the driving adapter hands the use
case an encoder, the use case records what the encoder makes of its result, and a replay returns
the recorded bytes for the adapter to decode (`korpis_app::idempotency`). The use case never sees
the wire format, and the adapter never sees the transaction.

**Errors.** Each layer has one error type and translates at its boundary: `sqlx` errors become
`StoreError` in the store, use-case refusals are `AppError`, and the API adapter turns those into
the structured `korpis.v1.Error` of §8 of `10-api.md`. Internal causes are logged where they are
translated and never reach the caller.

## 8. How it is tested, and what that costs

The domain is tested with plain values (`korpis-core/tests/`, unit tests in each module). The
store is tested against a real PostgreSQL in a throwaway database per test
(`korpis_store::testing::TestDb`), because the properties that matter there (uniqueness,
append-only grants, the plan-applying index) are enforced by the database and a fake would only
pretend to enforce them. Use cases are tested end to end through the API adapter against the same
real database (`korpis-server/tests/api/`), which exercises the translation, the use case, and the
store in one pass.

There is deliberately no in-memory fake of the store ports. A fake would make use-case tests faster
and independent of PostgreSQL, and it would be a second implementation of every query whose
semantics (transaction isolation, constraint behaviour) drift from the real one without anything
noticing. The cost is that use-case tests need the development database (`scripts/dev-pg.sh up`)
and take seconds rather than milliseconds. If the suite grows slow enough to matter, the remedy is
a contract test suite that both a fake and the PostgreSQL store must pass, not a fake on its own.
