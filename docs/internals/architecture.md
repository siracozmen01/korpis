# Architecture of the code

How the Rust workspace in `crates/` is organized, and the rules that keep it that way. What each
part does is on its own page; each crate's root documentation says what it holds.

## Contents

- [Ports and adapters](#ports-and-adapters)
- [The dependency rule](#the-dependency-rule)
- [Crates](#crates)
- [Where a new thing goes](#where-a-new-thing-goes)
- [What crosses a boundary](#what-crosses-a-boundary)
- [How it is tested](#how-it-is-tested)

## Ports and adapters

The control plane is a hexagon with three layers, and dependencies point inward only.

```
driving adapters (call into Korpis)          driven adapters (Korpis calls out)
  korpis-server (Connect API)                  korpis-store (PostgreSQL)
  korpis-cli                     ---->         korpis-driver-oci (containerd)
        |                    ports             korpis-driver-firecracker (KVM)
        v               (traits in app)              ^ implements
  korpis-app: use cases, ports  ---------------------+
    korpis-core: the domain
```

**Domain (`korpis-core`).** The nouns of the [glossary](glossary.md) and the rules between them:
identifiers, actions, scopes, grants and their algebra, effects, workloads, plans, placements. It
does no I/O, has no async runtime, knows nothing of PostgreSQL or the wire, and reads the time
only when a caller passes it. Everything in it is tested with plain values, and the same code runs
in the control plane, the agent and the relay, which the relay's authorization depends on.

**Application (`korpis-app`).** One use case per thing a client can ask for. A use case
authenticates and parses nothing: it receives a caller and domain values, authorizes through the
one authorization path, performs the change, and records the effect in the same transaction as
the change. What it needs from outside is a port, a trait such as `GrantStore` or `Clock`. It is
async but runtime-agnostic: it uses futures, never `tokio`, and never names `sqlx`, `connectrpc`
or a generated wire type.

**Adapters.** Everything that touches a socket, file, database or process. A driven adapter
implements a port; a driving adapter turns an outside request into a use-case call and holds no
business rule. The binary that wires adapters into use cases is the composition root, the only
code that knows every layer.

Authorization, effect recording and reconciliation must be identical whichever door a request
came through: the API, the agent stream, the CLI, the relay. A layer that cannot see a door
guarantees that. The cost is translation: a request crosses the wire, domain and storage type
systems, and each boundary has a mapping module to write and keep honest. That is accepted,
because letting wire or row types into the rules is how rules come to depend on transport
details.

The agent and the helper are separate programs with their own use cases. They share the domain
crate and the wire contract and nothing else. The helper does not link the domain's authority
module (the `authz` feature is off for it), an RPC stack or an async runtime: its attack surface
is the reason it exists.

## The dependency rule

`scripts/check-layers.sh` reads the dependency graph with `cargo tree` and fails when a row of
this table is broken. The compiler makes an inward dependency impossible to forget; the script
makes an outward one impossible to add quietly.

| Crate | May depend on (of ours) | Must never depend on |
|---|---|---|
| `korpis-core` | nothing | `tokio`, `sqlx`, `connectrpc`, `korpis-proto`, any I/O crate |
| `korpis-app` | `korpis-core` | `tokio`, `sqlx`, `connectrpc`, `korpis-proto` |
| `korpis-wire` | `korpis-core`, `korpis-proto` | `korpis-app`, `korpis-store`, `sqlx` |
| `korpis-store` | `korpis-core`, `korpis-app` | `connectrpc`, `korpis-proto` |
| `korpis-recipe` | `korpis-core`, `korpis-app` | `sqlx`, `connectrpc`, `korpis-proto` |
| `korpis-extension` | `korpis-proto` | every AGPL crate of ours, `sqlx` |
| `korpis-driver-conformance` | `korpis-proto` | every AGPL crate of ours, `sqlx` |
| `korpis-conformance-probe` | nothing of ours | every crate of ours, `connectrpc`, `hyper`, `sqlx` |
| `korpis-tls` | nothing | every other crate of ours, `connectrpc`, `tokio`, `sqlx` |
| `korpis-stream` | nothing | every other crate of ours, `connectrpc`, `tokio`, `hyper`, `sqlx` |
| `korpis-backup` | `korpis-proto` messages | every other crate of ours, `connectrpc`, `sqlx` |
| `korpis-install` | `korpis-core`, `korpis-confine`, proto and wire messages | the rest, `connectrpc`, `tokio`, `sqlx`, network clients |
| `korpis-files` | `korpis-confine`, proto messages | the rest, `connectrpc`, `tokio`, `sqlx`, network clients |
| `korpis-confine` | nothing of ours | every crate of ours, `connectrpc`, `tokio`, `sqlx` |
| `korpis-ingress` | `korpis-confine`, proto messages | the rest, `connectrpc`, `sqlx` |
| `korpis-egress` | nothing of ours | every other crate of ours, `connectrpc`, `sqlx` |
| first-party extensions | `korpis-extension`, `korpis-proto` | every other crate of ours, `sqlx` |
| `korpis-helper` | domain without `authz`, wire messages | `tokio`, `hyper`, `connectrpc` |
| `korpis-server` | all of the above | |

## Crates

- **Contracts:** `korpis-proto` (the `korpis.v1` schema and agent protocol, Apache-2.0),
  `korpis-containerd` (containerd's API, vendored), `korpis-extension` (the extension SDK),
  `korpis-driver-conformance` and `korpis-conformance-probe` (what a runtime driver must do,
  tested through the driver protocol alone).
- **Control plane:** `korpis-core`, `korpis-app`, `korpis-wire` (the one translation between
  domain and wire), `korpis-store`, `korpis-recipe` (the one reader and writer of the recipe
  format and its OCI layout, shared with the CLI so an author's tool and the server never
  disagree), `korpis-server` (API, TLS, keys, `install`, `serve`), `korpis-egress` (where requests
  made for a tenant may go).
- **Node:** `korpis-agent` (the supervisor), `korpis-helper` (the root helper), the drivers and
  `korpis-driver-kit`, `korpis-image` and `korpis-guest` (microVM drives and first process),
  `korpis-cdi` and `korpis-topology` (devices and cores as the node's own files and kernel report
  them), `korpis-confine` (what every job does first), `korpis-install`, `korpis-files`,
  `korpis-backup`, `korpis-stream`, `korpis-ingress`, `korpis-tls`.
- **First-party extensions:** `korpis-target-s3`, `korpis-target-fs`, `korpis-idp-oidc`,
  `korpis-kms-openbao`.
- **Clients and tests:** `korpis-cli`, `korpis-e2e` (real nodes end to end on one machine), and
  the panel in `web/`.

## Where a new thing goes

- A rule true whatever the transport and storage: `korpis-core`.
- Something a client or node can ask for: a use case in `korpis-app`, in its context's module,
  with the effect it records.
- A new question for the database: a method on the context's port in `korpis-app`, implemented
  in `korpis-store`.
- A new RPC: its schema in `korpis-proto`, its mapping and handler in `korpis-server`, calling a
  use case. A handler with an `if` that is not about the wire is in the wrong crate.

Within each layer, code is grouped by bounded context, with the same module name in every layer:
`tenancy`, `authz`, `audit`, `workload`, `recipe`, `placement`. Contexts talk through domain types
and use cases, never through another context's storage.

## What crosses a boundary

**Requests.** A driving adapter turns wire messages into domain values and refuses what does not
parse (a malformed identifier, a name with a control character, a label outside the label
syntax), so a use case never receives a string where a domain type exists.

**Stored data.** The store owns the format of what it writes: structured columns hold documents
defined in `korpis-store/src/stored/`, never serialized wire messages or domain structs, so a
renamed field changes a conversion, never data already written. That matters most for the effect
log, which is append-only. The one exception is the intent body, kept as the
`korpis.v1.IntentBody` message in canonical JSON so the schema's compatibility rules cover the
database too; the store reaches that format through the `IntentCodec` port and knows nothing of
protobuf, which is why test databases are built with the encoding the server uses.

**Idempotent replays.** A retried mutation returns the original response. The use case records
the response in the transaction of the change through an encoder the adapter hands it, and a
replay returns the recorded bytes; the use case never sees the wire format and the adapter never
sees the transaction.

**Errors.** Each layer has one error type and translates at its boundary: `sqlx` errors become
`StoreError`, refusals are `AppError`, and the API turns those into `korpis.v1.Error`. Internal
causes are logged where they are translated and never reach the caller.

## How it is tested

The domain is tested with plain values. The store is tested against real PostgreSQL in a
throwaway database per test, because uniqueness, append-only grants and the plan-applying index
are enforced by the database and a fake would only pretend to. Use cases are tested through the
API adapter against the same database, covering translation, use case and store in one pass.

There is deliberately no in-memory fake of the store: it would be a second implementation of every
query, drifting from the real one's transaction and constraint behavior with nothing noticing. The
cost is that use-case tests need the development database and take seconds. If that grows too
slow, the remedy is a contract suite both a fake and the store must pass, not a fake alone.

Where Korpis implements a standard itself, it is tested against an independent implementation of
the other side, in a workspace of its own under `conformance/` with its own lockfile, so what it
pulls in cannot change how the main workspace builds: `conformance/webauthn` drives the WebAuthn
relying party with 1Password's software authenticator, and `conformance/oidc` runs an independent
OpenID issuer.
