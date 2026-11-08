# Providers

How Korpis calls an [extension](extensions.md) that provides a capability, how failures and slowness
are contained, what each provider type answers, and how actions and UI slots reach every client.

## Contents

- [The extension dials out](#the-extension-dials-out)
- [Calls are queued in PostgreSQL](#calls-are-queued-in-postgresql)
- [Replacing a provider](#replacing-a-provider)
- [The SDK](#the-sdk)
- [Failure is declared per type](#failure-is-declared-per-type)
- [The breaker](#the-breaker)
- [Step providers](#step-providers)
- [Health providers](#health-providers)
- [Key management providers](#key-management-providers)
- [Block storage providers](#block-storage-providers)
- [Event consumers](#event-consumers)
- [Actions](#actions)
- [UI slots](#ui-slots)

## The extension dials out

A provider opens `ProviderService.Serve`, a bidirectional stream to the control plane authenticated
by its workload credential, and its first message, `Register`, names one of the `provides` entries
its installation recorded, with a version and a concurrency of 1 to 1,024 calls at once. The control
plane sends calls down that stream, each with an identifier and a deadline, and results come back on
it. This is the agent's direction for the same reason: what runs on a node dials out, so neither a
node nor an extension needs an address the control plane must reach. Several replicas register
several streams. Each registration and withdrawal is an effect (`provider.register`,
`provider.withdraw`).

The control plane runs as several replicas with no leader, so a stream is held by exactly one of
them and each registration records which. The holder confirms its registrations every five seconds;
one unconfirmed for thirty belongs to a replica that is gone and is withdrawn by whichever notices,
so for up to thirty seconds a dead replica's registrations stand and a call routed to one fails as
retryable.

## Calls are queued in PostgreSQL

The replica wanting a provider called is rarely the one holding its stream, so a call is written to
a queue with its deadline (at most five minutes), for the organization whose installation serves it:
the caller's own, else its nearest ancestor's, which is how a child's provider shadows its parent's.
Each replica claims, with `FOR UPDATE SKIP LOCKED`, the queued calls its streams may take: only a
stream of the version registered most recently for that provider, and only as many as its
concurrency leaves free, one task per replica claiming for all its streams so no free slot is
counted twice. The provider's result, the stream's end, or the deadline finishes a call; at the
deadline the provider is sent `CancelCall` and a late answer is discarded. Queueing and finishing
announce on one notification channel, and everyone re-reads every second, so a lost announcement
makes a call late, never stuck.

A call naming nothing any installation provides fails at once, `provider.unknown`, not retryable;
one whose stream ends or whose registration is withdrawn fails at once, `provider.stream_ended`,
retryable; one nobody answered fails at its deadline, `provider.deadline`, retryable. A provider
whose stream dropped is unavailable only until it reconnects within the deadline: a call queued
meanwhile waits. Every call is three transactions where an in-process call needs none, acceptable
because provider calls are control-path operations (a record published, a backup started, a step
resolved), not per-request traffic. Finished calls are kept an hour. A call's answer is a row, so a
secret in it (a backup target's scoped credential, a sign-in code) is struck from the row by its
caller in the transaction after reading it, and by the sweep a minute after for a caller that died.

## Replacing a provider

"Newest" is the version registered most recently, not the highest by any ordering of version
strings. A replacement registers beside the old version, new calls go only to it, the old one
answers what it already holds and its stream is then closed, so dependents see no failure window: a
drain applied to one registration. A rollback is the same operation. The cost: a newer registration
whose replica died attracts every new call for up to thirty seconds, and those wait even though the
older version is still connected.

## The SDK

A Rust extension links `korpis-extension`. It fetches the credential from the identity socket and
renews it five minutes before expiry; follows its installation's subscriptions, acknowledging each
event only once the handler returned, so what the handler failed on is delivered again; and serves a
provider stream, answering `Ping` itself, running other calls at most `concurrency` at a time,
dropping a call at its `CancelCall`, and reconnecting with a backoff of one second doubling to
thirty, so a replica going away costs one reconnection. It stops only when refused. In a container
it reaches the control plane through `/run/korpis/api.sock`, which its node relays, and over the
network otherwise, so the same extension runs in a container and, in a test, on the host.

## Failure is declared per type

Every call has a deadline, and what a type's failure means for its dependents is declared in the
domain (`ProviderType::failure_policy`), so every call site reads it rather than deciding afresh:

| Type | Its dependents, when it fails |
|---|---|
| `dns`, `backup_target`, `edge_router` | the dependent thing is `failed` in the workload's status, the provider and reason named; reconciliation retries; nothing else is blocked |
| `identity`, `key_management` | fail closed: what it would have asserted, or the key it would have opened, is refused, and nothing stands in; failing open would be an authentication bypass |
| `step` | the install step fails with its reason, and the plan stops there |
| `health` | the health it judges is `unknown`, never `unhealthy`, since treating a failed prober as a verdict would restart every workload it probes |
| `console`, `chat_adapter` | the request that needed it fails as unavailable and may be retried |
| `address_router` | fail closed for a move: no node holds an address the router has not said it routes there, and what it routes stays |
| `block_storage` | fail closed for an attach: a workload whose volume is not attached does not start; one running keeps its device |

An answer that strays outside what the contract allows (a path climbing out, content without a hash,
access for another prefix) is `provider.malformed`, counted by the breaker, and never reaches a
node.

## The breaker

A provider failing consistently would otherwise have every reconciliation loop in the fleet queue
behind it, and the expensive failure is one that works: a DNS provider answering correctly at ninety
percent of a five-second deadline, every time, produces no error while consuming the reconciler's
throughput. A deadline bounds one call and says nothing about aggregate cost. So the breaker trips
on latency as well as failure, and concurrency is bounded per provider, so one slow dependency
occupies a slice of capacity, not all of it.

Each provider (by the serving organization, type and name) has one breaker in PostgreSQL, since any
replica may call any provider. **What evaluates it:** the caller of each call, after it finishes,
holding the breaker's row lock, judging from the queue itself, the provider's last twenty calls
finished within five minutes, so the evidence is never a second copy that could disagree. A call
counts as failed if it failed retryably (deadline, stream ended, provider saying retry may help); a
provider's answer that the request is wrong is a correct answer. With at least ten calls judged, the
breaker trips when half failed, or the median call took four fifths of its deadline or more,
queueing included. It opens for ten seconds, during which a call fails at once,
`provider.circuit_open`, retryable with when to retry, and is never queued. Then the next call is
the probe and others are refused until it finishes: a probe neither failing nor slow closes it;
anything else reopens it for twice as long, up to five minutes. A probe whose caller died is
replaced after its deadline. Tripping writes `provider.degraded` and closing `provider.recovered`
([observability](observability.md#conditions)). `ProviderService.ListProviders` publishes every
breaker and its measurements.

Each call takes two more transactions, to be admitted and judged, and every caller of one provider
takes its row lock briefly. A provider called fewer than ten times in five minutes is never tripped;
its failures reach callers as they happen. A breaker over a provider nobody calls stays as last
judged, at no cost. Thresholds are constants until a deployment shows a reason for others. A
provider a tenant installed is bounded by its grants, so a slow one degrades that tenant's
reconciliation and nobody else's; a fleet-wide provider is one the operator installed, the price of
core using the same mechanism.

## Step providers

`RunStep` carries the step kind, its arguments, the workload and the node's platform. The answer
lists at most a thousand artifacts, each a relative path beneath the step's `to`, an `http` or
`https` URL, its SHA-256, size and mode, and whether it is an archive to unpack under `extract`'s
rules, with `pinned` and what it `resolved` to. A step provider never writes a tenant's volume: the
workload's own worker writes what it resolved, so re-running a step can only re-resolve and
overwrite the same targets. Its argument schema is the `provides` entry's `arguments`, restricted to
`string`, `int` and `bool`: a step runs under the extension's grant, not the recipe author's, so a
resource argument would hand the author's choice of object to authority the author does not hold.

**Who calls it.** The node running the install asks over its session (`StepRequest`: plan, step,
action, platform), and the control plane answers on the same stream, since a provider has no stream
to a node and the control plane alone holds both the node's question and the provider's
registration. It answers only the node the workload is placed on, only for an install step of an
open plan, only through the installation the intent pinned (`step.not_assigned`, `step.not_a_step`,
`step.provider_changed`, none retryable). An unpinned result sets the workload's `unreproducible`
mark before the answer is sent. The pin is checked and then the call made, so an upgrade landing
between them is answered by the upgraded installation once. On the node, an install asks for every
provider step and starts its worker only once all are answered, writing the answers beside its
progress, so a resumed or retried install runs the same actions whatever a channel holds by then;
the node checks the answer again before using it.

## Health providers

A health provider answers a `query` probe, and the node, not the provider, reaches the workload: the
node holds the socket, opened by the helper inside the workload's namespace, and the provider holds
the protocol. After each round the node asks the control plane what next (`HealthQuery`), the
control plane calls the provider (`HealthStep`, with the protocol, the workload, the endpoint, its
transport, and every round so far: sent, read, and whether the workload closed), and answers the
node with the next round (what to send, how much to read within how long) or the verdict, with a
reason and what the workload said of itself. At most four rounds, 4 KiB sent and 64 KiB read a
round, ten seconds a round within the probe's timeout; beyond is `provider.malformed`. The control
plane answers only the workload's node, for a query probe of the intent it names, through the pinned
installation (`health.not_assigned`, `health.not_a_query`, `health.provider_changed`); each of
those, the provider's own failure and an open breaker are answered as unjudged.

A provider given its own way to the workload would need either a tenant-private action an operator's
grants do not carry into a customer's organization, or the endpoint's public address, which the
egress default drops on a single machine and which an unexposed endpoint lacks. Directing the
exchange gives the provider one query's bytes and nothing to keep. The cost is the queue: every
round is a call of three transactions, so a thousand servers probed every fifteen seconds with
two-round queries are four hundred transactions a second, and an exchange needing more than four
rounds or a held connection cannot be a health provider's.

## Key management providers

A key management provider holds a repository's key-encryption key for its tenant: it makes and wraps
a workload's data key, and opens a wrapped one, for one context, `<repository>/<workload>`. **It
never answers with a key in the clear.** Every call names a recipient's one-time X25519 public key,
and the answer is the data key sealed to it with HPKE (RFC 9180, base mode, DHKEM(X25519,
HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305), the context bound in. Answers travel the stored
queue, so this is what keeps a data key out of the database, its write-ahead log and its backups.
The SDK seals; an implementation returns the key and never handles a recipient. See [backup
keys](backup-format.md#custody).

## Block storage providers

Its calls are `CreateBlockVolume` (a size, answering the provider's own name), `AttachBlockVolume`
(one volume to one node for one lease epoch), `DetachBlockVolume`, `DeleteBlockVolume` and
`DescribeBlockVolume` (size, and which node may write). **It fences before it attaches**: `attach`
first refuses the volume to every other node in the storage itself, then answers what the node
needs, the protocol (`iscsi`, `rbd`, or a test `loop`), the target, and a credential scoped to the
volume where the storage can scope one. An `attach` for an epoch lower than one already attached is
refused, so a retried call from before a failover cannot take the volume back. A provider registers
its `fencing` (`storage_enforced` or `lease_only`), which any other type is refused for saying, and
the control plane takes it as the class's. An attachment answers only in a protocol the node speaks,
with a target that protocol takes; for a loop device a bare file name, never a path, so a provider a
tenant installed cannot have a node attach a host file. The credential is never logged and is struck
from the stored call. See [network storage](network-storage.md).

## Event consumers

A consumer subscribes to the event stream, filtered by grant; delivery is at-least-once, ordered per
key (per workload), with a gap marker for a position past retention
([observability](observability.md#subscriptions)). Across workloads there is no order, since none is
meaningful and one would let a slow workload's events hold up everyone's.

## Actions

An extension registers actions in `ext.<name>.<action>`. The web, chat and CLI surfaces are
generated from the schema, so a registered action appears in all three (a button, a slash command, a
subcommand) with autocomplete, authorization, plan rendering and audit attached: in every competing
product, an extension wanting a Discord command writes a Discord bot. The CLI's is `korpis extension
invoke <ext_…> [<action>] [--arg name=value]…`, which lists the actions without one.

An installation whose document declares actions registers an `action` provider under its own
identifier, implied by the document rather than written in its `provides`, so a stream serving them
is held and called like any other provider's, and no installation, however it is named or wherever
installed, can claim another's: the control plane sets the name of an `action` registration to the
installation the stream authenticated as. `ExtensionService.InvokeAction` takes the
installation, the action and its arguments, which are checked against the declaration (every
required one given, none undeclared, each of its type, at most 64, text at most 4 KiB), and queues
the call with a deadline of a minute. The answer is a line for the person (at most 4 KiB) and data
for a client to render (one JSON value, at most 64 KiB), or the extension's failure.

**An extension acting for someone acts with the intersection, never its own grant.** If the gateway
checked the invoker's `ext.rcon.kick` and the extension then kicked under its own broader grant, a
user authorized on workload A could pass workload B: the confused deputy. So an action's schema
declares which arguments are resources, typed (`workload`, `project`), and the gateway authorizes
the invoker for the action on each, or on where the extension is installed when it takes none. A
resource must lie where the extension is installed: a grant of `ext.rcon.kick` reaching further,
which another installation of that name could serve, does not carry this one beyond its own place. The
extension receives an invocation token, `korpisiv_…`, presented in the `korpis-invocation` header
beside its own credential: a call carrying it is authorized by the extension's own grants, but only
on those objects and what lies beneath them, and refuses anything no ticket could name, such as
issuing a grant. The control plane keeps the token's digest, accepts it only from the installation
invoked, and stops accepting it when the invocation finishes or a minute after it began, whichever
is first. The invocation is an effect, `extension.invoke`, by the invoker about where the extension
is installed, naming the action and its outcome; every effect made with the token names the
invocation, so the invoker, the extension and what it did are joined. What the extension does on
its own initiative, under its standing grant, is attributed to it alone.

## UI slots

Declared, typed and named: an extension contributes to a slot and never edits the bundle. A
contribution is declarative, `{"kind": "card" | "form", "action": <one it declares>, "title": ...}`,
rendered by the panel's own components, so it inherits their theming, accessibility, mobile layout
and three-state rendering of unknowns, and runs no code in the panel's origin: it cannot read the
viewer's credential or draw a convincing fake of the panel's own screens. A card shows what its
action answers, asked as it is shown and again when the viewer asks; a form lays out the action's
arguments and invokes it on submit. Either goes through `InvokeAction`, authorized for the viewer
as every invocation is, and `ExtensionService.ListSlots` lists for an object only the contributions
whose action the viewer may invoke there, so a slot never offers what its viewer cannot do. The
slots are `workload.panel`, on a workload's page, whose action takes a workload and is given the
page's, covered by what is installed in the workload's project and in its organization and every
organization above; and `organization.settings`, covered by what is installed into the organization
itself. A slot is added only for an extension that needs it. A document contributing to another
slot, or with a descriptor that does not read, is refused when it is loaded.

Custom UI, an iframe on a separate origin under a restrictive CSP talking over a typed `postMessage`
channel with its own token scoped to the intersection of the extension's grants and the viewer's
authority for the resources in view, is not built (#146): a document whose descriptor is a URL is
refused saying so, until it is.
