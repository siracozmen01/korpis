# API

The one schema every client and every node speaks, its encoding and compatibility rules, the action
vocabulary grants name, and how mutations, watches, errors and resynchronization behave. Rate limits
are on [rate limits](rate-limits.md).

## Contents

- [One schema](#one-schema)
- [Two surfaces](#two-surfaces)
- [Encoding](#encoding)
- [Compatibility](#compatibility)
- [Actions](#actions)
- [Mutations](#mutations)
- [Streams and watches](#streams-and-watches)
- [Errors](#errors)
- [Resynchronization is nearly free](#resynchronization-is-nearly-free)
- [Publication](#publication)

## One schema

Pterodactyl has two APIs, Application and Client, that overlap, disagree, and between them cannot do
some things at all: what happens when a web interface is built first and an API added beside it.
Korpis has one schema, and the panel, the chat client, the CLI, extensions and third-party
integrations are all clients of it, none able to do anything another cannot. The server, every SDK,
the OpenAPI document and the reference are generated from it, so "the panel can do something the API
can't" has nowhere to live.

## Two surfaces

| | Client API | Agent protocol |
|---|---|---|
| Consumers | web, CLI, chat, extensions, integrations | node agents |
| Reach | anyone | machines the operator runs |
| Stability | strong, with published deprecation windows | effectively frozen |
| Encoding | Connect: JSON and binary on one endpoint | binary, streaming |
| Authentication | grants and capability tokens | the node's key |
| Evolution | additive; breaking changes take a new version | additive only |

They are separate because their obligations differ: a client API can deprecate over eighteen months
and clients update, but an agent on a machine Korpis does not control, in a rack its operator visits
twice a year, must keep working. So the control plane supports every agent protocol version shipped
by a still supported release, never fewer than the current and the two before; a protocol version is
not a release version, and the binding case is the LTS, whose agents a control plane of any
supported release serves for the LTS's life. Upgrade order is the control plane first, then agents
one at a time; a mixed fleet is a supported state.

## Encoding

**Protocol Buffers served over Connect.** One definition generates the Rust server and agent, the
TypeScript client, other languages' clients, the OpenAPI document and the reference; its
compatibility rules are precise and mechanically checkable; streaming is native; and the agent
protocol carries enough observations and stream data for wire efficiency to matter. Connect rather
than raw gRPC because it serves Connect, gRPC and gRPC-Web on one endpoint, passes ordinary HTTP
proxies and CDNs, and is reachable from a browser without a translating proxy.

The schema lives in `crates/korpis-proto/proto/`, in `korpis.v1` (the model and client services),
`korpis.agent.v1`, and `korpis.driver.v1` and `korpis.helper.v1` (the node's internal protocols).
`buf lint` and a committed descriptor set guard it, and the Rust code is generated from the
descriptor set, so building needs no protobuf compiler.

**Encoding is deterministic.** Every `map` is generated as an ordered map, so a message encodes to
the same bytes every time: an assignment is signed over its exact bytes and compared by digest, and
with an unordered map every comparison would read as a change. A third-party agent needs no such
property; it verifies the bytes it was sent.

**Times are `google.protobuf.Timestamp`**, and a time a client sends before 1970 or after 9999 is
refused, naming its field, so no value a client writes is one the control plane cannot store.

**The JSON surface is first class.** The ecosystem Korpis needs (billing integrations, chat bots,
monitoring scripts) is written by people who reach for `curl`, and one that needs a protobuf
toolchain to send a request does not form:

```bash
curl -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
     -d '{"workload_id":"wl_7f3a"}' https://korpis.example/korpis.v1.WorkloadService/Restart
```

## Compatibility

| Never | Always |
|---|---|
| change a field number | reserve removed numbers, permanently |
| change a field's type | add fields as optional, with a meaningful zero value |
| rename a wire field | tolerate unknown fields and enum values |
| repurpose a reserved number | give every enum `UNSPECIFIED = 0` |
| add a required field | treat absent as unchanged in updates, never as clear |

**A field's default never changes.** Intent bodies are stored with defaults emitted, which freezes
the meaning of rows written; for every consumer, a client generated against `v1` and a later control
plane agree on what an absent field meant only if the answer never moved. Changing a default is a
breaking change; adding a new field with the new default is always available.

A breaking change produces `korpis.v2`, and `v1` is served for a published, dated period.
Deprecation is announced, then warned in response metadata, then removed after the window, so
nothing disappears without having warned in band. Before the first release the wire changes on
design merit alone.

**Capabilities are negotiated, never inferred from a version.** An agent's `Hello` carries its
capabilities and a client reads `ServerInfo`. No component branches on a version to decide what
another can do: two builds of one version on different kernels differ, so a version is a fact about
a build.

## Actions

Grants name actions, so the vocabulary is part of the contract, under one rule: **actions are never
renamed and never split.** Splitting `workload.write` into finer actions would silently change what
every existing grant permits, widening or narrowing authority someone deliberately issued: an
authorization bug shipped as a refactor. New granularity arrives as new finer actions the coarse one
implies, the coarse one deprecated for new grants and honoured forever for old ones; **a capability
no existing action covered is a new action implied by nothing**, so introducing it widens no grant.

```
workload.read · create · write · delete · start · stop · restart · console.read · console.write
workload.exec · file.read · file.write · secret.read · classify · migrate
extension.read · install · uninstall          event.read          metering.read
plan.read · approve · cancel
volume.read · snapshot.create · snapshot.rollback · snapshot.delete · delete
endpoint.read · write · release
backup.create · browse · read · restore · prune      repository.read · write
domain.read · write · attest      certificate.read · write      portal.read · write
address_pool.read · write · attest      edge_group.read · write      stream.share
grant.read · revoke      template.write      identity.binding.write · relay
org.read · create · write · suspend · resume · quota.write
project.read · create · write · delete
node.read · enroll · cordon · drain · evacuate · place · remove
ext.<extension>.<action>     an extension's own, namespaced, attenuated and audited as core's
```

- **`workload.write` declares intents** (configuration, resources, runtime, endpoints) and implies
  no power action, so "may edit the configuration" and "may stop the server" are delegated apart.
  `workload.exec` (a new process inside) is separate from and stronger than
  `workload.console.write`.
- **A run needs no action of its own**: a higher power cycle requests it, under `workload.restart`;
  `CancelRun` is `workload.stop`; its record `workload.read`; its streams `workload.console.read`. A
  new action would have left every grant that may restart a server unable to run its backup now.
- **`org.suspend`, `org.resume` and `org.quota.write` act from above**, authorized over the
  organization's parent, so a tenant cannot lift its own suspension or raise its own quota.
- **`workload.classify`** sets the operator's layer of [classification](plans.md); implied by
  nothing, `workload.write` included, since relaxing a step is choosing which changes a person sees
  first. **`workload.migrate`** proposes, abandons, rolls back or releases a migration, implied by
  nothing since a migration takes capacity on a second machine; naming the node needs `node.place`
  there too. **`node.place`** binds a workload to a node by hand. **`node.remove`** is not implied
  by `node.cordon`, which uncordoning undoes.
- **`metering.read`** reads what a workload, project or organization used, separate from
  `workload.read` so a billing system reads usage and nothing else, kept while an organization is
  suspended since it is still billed.
- **`repository.*`** decides where backups go and who holds their key; taking a backup is
  `backup.create` beside `repository.read`. A backup's record is `workload.read`; its tree and
  contents `backup.browse` and `backup.read`, private actions; restoring into another workload's
  volume is `backup.read` there and `workload.file.write` on the other.
- **`domain.attest` and `address_pool.attest`** vouch for what nothing in the protocol can prove, so
  they are operators' powers held on the root organization. `domain.write` creates, verifies,
  deletes and delegates domains; `certificate.write` uploads provided certificates, whose keys no
  action reads back. `portal.*`, `edge_group.*` and `address_pool.read`/`write` are their
  organizations' own.
- **`template.write`, `identity.binding.write` and `identity.relay`** are implied by nothing: a
  binding decides whose word makes someone a subject; relaying passes on what a person presented to
  a provider and is handed a ticket for one action, sent as the `korpis-as` header of the one call
  it permits, the person's own grants deciding; what no ticket can name is refused as
  `identity.relayed`.
- **`extension.install`** is held on the target installed into and, when the extension provides
  anything, on the organization, since a provider serves its whole subtree; it grants the extension
  nothing by itself. `event.read` on an object is what an event about it is delivered through.
- **Delegating is not an action**: whether a grant may be attenuated, how deep and with which
  actions, is a property of the grant, checked against the chain. `grant.revoke` and `grant.read`
  remain actions, so "who can restart this server" is answerable by someone.
- **Irreversible actions are marked in the schema**: `workload.delete`, `volume.delete`,
  `volume.snapshot.rollback`, `volume.snapshot.delete`, `endpoint.release`, `backup.prune` and
  `node.remove`. The mark is what set-subject and link grants exclude by default, and what the chat
  client and panel route to an explicit confirmation. This is the one list.

## Mutations

Every mutating call carries an `idempotency_key`, so a client that times out and retries never
applies twice, and an `expected_version`, a compare-and-swap against the object's version:
concurrent declarations are detected, not merged, and a mismatch is `CONFLICT` carrying the intent
that won, since a chat command and a web user editing one workload at once is normal. Last write
wins is never the behaviour.

**A secret in a response is not recorded for replay**, since a retry returns the original response
from a stored record and a secret there would be readable by anyone reading the database. A mutation
answering one (an enrollment token, a token holding a grant) records its response without it, and a
replay returns the secret empty; the client asks for a new one, the lost enrollment token expiring
unused and the lost grant token disarmed by revoking its grant. The sign-in ceremonies, exchanging a
refresh credential and signing out carry no idempotency key: each spends a single-use proof, so a
retry after a lost answer is refused rather than applied twice.

**Read your writes.** Every write response carries a `consistency_token`, which a client passes back
on reads so they go to a replica that has caught up or to the primary; the SDKs carry it, so an
operator pressing a button never sees the change vanish on refresh.

## Streams and watches

Console, logs and events are server-streaming, console input client-streaming. A gap is always
visible: console and log output drops on overflow of a bounded buffer and emits a gap marker with
the range and duration lost; console input blocks and is never dropped or reordered; events are read
from the durable log at the consumer's pace, a position past retention answered with a gap marker. A
log with an invisible hole is worse than none, because someone concludes from absence that nothing
happened. Streams are durable and seekable, so a reconnecting client resumes at an offset. See
[streams](streams.md).

**A status watch is level-triggered and needs no offset.** `WatchWorkload` sends the status now,
then each that differs from the last sent; every message is the whole status. Each change is
announced by the database when its transaction commits, so a writer cannot forget to announce. An
announcement is a wake-up, not a record: a watcher that may have missed some re-reads, and every
watcher re-reads on an interval anyway, since time alone changes some statuses. Every read uses the
watcher's authority as it is then, so a grant revoked during a watch ends it with the refusal a
fresh read would get. The cost is a listening connection per control-plane process and a read per
announced change per watcher.

## Errors

```
Error
  code         INVALID | UNAUTHENTICATED | DENIED | NOT_FOUND | CONFLICT | UNSATISFIABLE
               | QUOTA_EXCEEDED | OVERFLOW | UNAVAILABLE | INTERNAL | RATE_LIMITED
  reason       a stable identifier, such as quota.memory.exceeded
  field        a path, for validation errors
  detail, retryable, retry_after
```

Structured and machine-readable, never a message string clients parse. A transaction the database
undid so another could finish (a deadlock or a serialization loss) is `CONFLICT`, `store.contended`,
retryable, its idempotency key making the retry safe.

**Existence is gated by read authority.** A subject who can read a resource but lacks the action
gets `DENIED`; one who cannot read it gets `NOT_FOUND`. `DENIED` for something unseen would confirm
it exists, across a tenancy boundary leaking workload names that are often customers' names.
`DENIED` names the missing action, never which grant would have granted it, which would map the
authorization structure.

**A list's page token names nothing.** A list filtered by read authority reads past what the caller
may not see until its page is full, nothing is left, or eight pages of rows are read, and its token
is the resume row sealed under a key every control plane shares, with the list's kind bound in. So a
token is opaque, continues on any replica, and is refused by another list; a page ending on another
tenant's object does not hand over its identifier, and a caller who sees little of a long list
cannot make one call read all of it. A page may come back short, even empty, with a token, and how
long a call takes still says roughly how much it read.

## Resynchronization is nearly free

The protocol is level-triggered, so the control plane sends a node the complete set of its
assignments, which for hundreds of workloads on every reconnect would be expensive. So the set
carries a digest over its canonical encoding, sorted by workload, covering each assignment's
generation:

```
agent -> Hello { set_digest }
         matches      -> InSync               nothing more
         known prior  -> AssignmentDelta      only what changed
         unknown      -> AssignmentSet        the full set, the safe path
```

The agent is never required to have received an earlier message, the unknown path always works, and
any inconsistency resolves to a full set, so the digest can be discarded at any time without
affecting correctness, which is what makes it safe. It is a set-level checksum; assignments keep
their generations as identity. The control plane keeps no history of earlier digests yet, so a node
that missed a change receives the whole set; `AssignmentDelta` is in the schema and not built.

## Publication

A third party must be able to write a conforming agent without reading Korpis's source, so these are
published as a separate Apache-2.0 artifact: the `.proto` definitions for both surfaces and for the
node's internal protocols (supervisor, helper, filesystem workers, drivers, relays); the generated
OpenAPI 3.1 document; these compatibility rules as a normative specification; and conformance
suites, as the [driver suite](driver-conformance.md) is. A protocol without a conformance suite is
documentation; with one it is a contract an ecosystem can build on instead of forking. Aggregate
reads (a dashboard fetching a workload, its observation, endpoints and recent effects in four calls)
are answered by purpose-built endpoints rather than a second, GraphQL query surface with its own
authorization and cost story.
