# Streams

**Status:** design **Date:** 2026-08-07 **Depends on:**
[`02-architecture.md`](./02-architecture.md), [`06-storage.md`](./06-storage.md),
[`10-api.md`](./10-api.md) **Implements:** Principles P4, P5, P6

---

## 1. The console is evidence

In Pterodactyl the console is a websocket into a memory buffer. Reload the page and it is gone. Ask
what a server printed before it crashed and the answer is a file on a node, reachable by whoever
has node access. Ask who typed `stop` and there is no answer at all.

That is not a missing feature; it is a category error. **A console is not a widget. It is a
record.** Support reconstructs incidents from it, moderation adjudicates from it, operators debug
from it, and in a multi-tenant system it is the primary evidence of what a tenant's software
actually did.

A `Stream` is a durable, ordered, seekable sequence of records produced by a workload or by Korpis
itself. Console output, log files, build output, and agent progress are all streams, differing in
retention and format rather than in kind.

---

## 2. Records, not lines

```
Record
  stream_id   the stream this belongs to
  offset      monotonic, per stream, the seek primitive
  time        node clock, at write
  source      stdout | stderr | file:<path> | korpis | input
  kind        text | structured | gap | control
  body        bytes
```

Offsets rather than timestamps are the identity, because node clocks step, and a reader that seeks
by time cannot express "resume exactly where I stopped". A client reconnecting sends its last
offset and receives what follows it, the same level-triggered discipline the agent protocol uses
(§4.1 of `02-architecture.md`).

`kind: structured` exists because a workload emitting JSON lines has already done the hard part,
and flattening that into text to re-parse it downstream is destructive. Structured records survive
intact and are exported as structured to `15-observability.md`'s consumers.

`kind: control` carries PTY events (resize, signal, EOF) because not every console is line-oriented
and a terminal that cannot be resized is a terminal that renders game server menus wrong.

---

## 3. Gaps are records

A record with `kind: gap`, carrying how many records and how much time were lost, and why.

This is P4's sharpest application. Every log system in existence drops under pressure; almost none
say so, and the result is that someone reads a log with an invisible hole in it and concludes from
the silence that nothing happened. **A log that lies by omission is worse than no log**, because it
is trusted.

```
[14:32:41] Saved the game
── 41,208 records dropped over 6s (write rate exceeded stream limit) ──
[14:32:47] Player Alex left the game
```

The marker is a first-class record: it has an offset, it persists, it appears in exports, it
renders in the web console and in Discord, and it survives replay.

---

## 4. Where a stream lives

Three tiers, read as one merged view:

| Tier | Holds | Latency | Purpose |
|---|---|---|---|
| node ring buffer | seconds to minutes | sub-millisecond | live attach, fan-out to readers |
| node segment files | the retention window | milliseconds | scrollback, replay, search |
| offload target | sealed segments, and beyond the window | seconds | survives the node, long-term evidence |

The agent owns all three. The control plane is not in the data path: a hundred consoles attached to
a hundred workloads on ten nodes is ten agents fanning out locally, joined to their readers by
stateless relays (§3.1 of `02-architecture.md`), not a hundred streams through the control plane.
A reader holds a short-lived capability token naming the stream, which the relay and the agent
both verify, the same pattern as file transfer (§8 of `11-surface-web.md`).

> Second external review, finding S19. Recorded in §17 of `23-walkthroughs.md`. This section said
> readers were "directed to the node", which under §3 of `02-architecture.md` has no inbound port.
> The relay is what makes "not through the control plane" and "the node dials out" true at once.

**Offload is on by default wherever an offload target exists, and segments are offloaded as they
seal, not when they age out.** A node that dies takes with it only the segment still being
written. Where no target is configured (a single machine with no object storage), the interface
says how much of each stream exists only on its node, because §3.6 of `01-model.md` promises
durability per failure and this is the failure it does not cover.

**The control plane holds the index of segments, never their bytes.** Each seal and each offload
is a record in the agent's journal (§4.10 of `02-architecture.md`), ingested like any other, so
the control plane knows every segment's offsets, digest, and where it is held: the `segments` of
the `Stream` in §3.6 of `01-model.md`. That index is what answers the question offload exists
for. When a node is gone, its offloaded segments are found from the index rather than by listing
the offload target, and a reader is told which offsets were held only on the node and are lost,
as a gap (§3). What it costs is two journal records per segment, one at its seal and one at its
offload, so the index grows with the number of segments and never with the number of records.
The index arrives with streams in Phase 1, and is in
the schema from Phase 0 by rule E of `20-roadmap.md`.

**A stream is part of the workload's durable state, so it migrates with it.**

> Finding 3 of `23-walkthroughs.md`.

Segments travel in the same incremental transfer as the volume during `replicate` and `final_sync`,
and the stream's final offset crosses at `cutover` alongside the lease epoch (§8 of
`05-scheduling.md`). The destination continues the offset sequence rather than restarting it,
otherwise every reader's stored position would silently come to mean something else, and §2's claim
that offsets are the seek primitive would be false across the one event most likely to make someone
go looking. Anything that could not be transferred becomes a gap record.

Segment files live in the tenant's storage and **count against the tenant's quota** (K-3). This is
not bookkeeping pedantry: a workload that logs at 50 MB/s is a workload that fills a node, and a
"log retention" setting that is not backed by an enforced quota is exactly the unenforced limit P4
forbids. The stream's own limits are declared per workload and enforced in the write path.

---

## 5. The workload is never blocked

When a workload writes faster than the stream can persist, there are three choices: block the
writer, drop, or spill without bound. Blocking is what a naive implementation does, and it means a
logging burst freezes a game server, the platform causing an outage in the thing it exists to keep
running.

**Korpis never blocks the workload on its own logging.** It drops, and it emits a gap record.
Spilling without bound is refused for the same reason unenforced quota is: it converts a workload's
bug into a node-wide outage.

The rate is a declared field with a sane default. A workload that legitimately produces high-volume
output declares a higher limit and pays for it out of its own quota, which is the honest way to
make that trade rather than discovering it at 03:00.

---

## 6. Input is an Effect

Console input is a record, and it is also an `Effect` (§3.3 of `01-model.md`), append-only, naming
the authorizing grant.

```
[14:41:03] input  "stop"     subject sub_9a2c (discord:steve#…)  grant g_7f3a via @moderator
```

"Who stopped the server" is one of the most-asked questions in this market and none of the
incumbents can answer it, because their console input is a websocket frame with a session cookie
behind it and no record afterwards. Here the answer is a row, and it is a row for the same reason
every other authority-bearing action produces one, not because a console-auditing feature was
written.

`workload.console.write` is a separate action from `workload.console.read`, so watching and typing
are independently delegable. Reading a stream a viewer is not entitled to read returns `NOT_FOUND`,
not `DENIED` (§8 of `10-api.md`).

---

## 7. Redaction happens before persistence

Game servers print their own configuration on startup. Configuration contains RCON passwords, API
keys, and database URLs. This is not an edge case; it is the default behaviour of a large fraction
of the software this platform exists to run, and every panel in this market persists those secrets
into a log that support staff can read.

Recipes declare which config fields are secret (§5 of `09-recipes.md`). The agent applies those
values as literal patterns **at write time, on the node, before the record is persisted**, so the
secret never reaches a segment file, an offload target, a backup, or an export.

```
[14:30:02] RCON enabled, password ████████ (redacted: rcon_password)
```

Stated honestly, per P4: **this is best-effort and is labelled as such in the interface.** A
workload that base64-encodes its own secret before printing it defeats pattern matching, and no
amount of cleverness fixes that. What it does defeat is the overwhelmingly common case, a program
echoing a value verbatim, and the difference between catching that and catching nothing is most of
the practical risk. Operators can add their own patterns; recipe-declared ones cannot be removed by
a tenant, because the tenant is not always the party being protected.

---

## 8. Sharing and replay

A stream range is a scope, so sharing one is a grant:

```
korpis stream share mc-survival --from 14:30 --to 14:45 --expires 7d
→ https://panel.example/s/st_4b19#g_c22e…
```

The recipient sees exactly that range of exactly that stream, with no account, and the link stops
working on its date. This is the operation support teams currently perform by pasting a screenshot
into a ticket, and it is possible here for the same reason the workload share link is (§6.1 of
`08-identity.md`), not as a stream feature, but because authority is attenuable.

Replay is a seek. The web console, the Discord window, and `korpis console --at` are all views onto
the same offsets, so "what did it look like at the moment it crashed" is a query rather than a
reconstruction.

Export is to open formats. §2 of `00-overview.md` says Korpis is not a monitoring stack and will
not build log search, alerting, or long-term storage; streams ship to Loki, OpenTelemetry, or
object storage and stop there. Substring and regular-expression search **within a workload's own
retention window** is provided because it is a property of the segment files rather than a search
product, and because sending an operator to a separate system to find one line is absurd.

---

## 9. Open questions

1. **Offload ownership. Resolved here, by the backup mechanism.** Whether the agent uploads to the
   offload target directly (fast, but the node holds object-storage credentials) or via a broker
   (slower, no node-held credential) is the same trade §7 of `02-architecture.md` resolved for
   registry pulls with short-lived scoped tokens.

It is resolved exactly as backup is (§5.5 of `06-storage.md`): the node receives, per offload job,
a short-lived credential that can write new objects under that workload's stream prefix and nothing
else, and segments are encrypted with the workload's data key before they leave the node. A target
that cannot issue such a credential is written through a relay, which holds the credential instead.
The node never holds a credential that reaches another workload's data.
2. **Multi-line and interleaving.** A Java stack trace is one event across forty records, and
   stdout and stderr interleave non-deterministically. Whether Korpis attempts reassembly, and
   risks being wrong, or presents raw records and lets consumers decide, is unsettled. Leaning
   toward raw, since §2's `structured` kind gives well-behaved programs the correct path already. →
   here
3. **Retention versus evidence.** A tenant may want short retention; an operator investigating
   abuse wants long. These conflict, and resolving it by letting the operator override tenant
   retention has privacy consequences that belong in the threat model. **Resolved in §8 of
   `17-security.md`**, a disclosed operator retention floor.
4. **Cross-workload correlation.** Following a request through a workload and its declared
   dependencies (§5 of `07-networking.md`) requires trace propagation, which is an
   OpenTelemetry-shaped problem rather than a stream-shaped one. → `15-observability.md`
5. **Very large single records.** A workload printing a 200 MB line is hostile input, and the limit
   must be enforced in the write path with a gap marker rather than discovered by an out-of-memory
   kill. The number is not chosen. → here
