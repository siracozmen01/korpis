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

A text record is the bytes as written, its line end included: `\n`, or `\r\n` from a terminal.
A line still unfinished when the writer flushes it, a prompt waiting for input, is a record of its
own with no line end, so a reader reproduces the output exactly and can tell a finished line from
a prompt. Added 2026-10-14, when the console was built: records first dropped their line ends,
which made every flushed prompt indistinguishable from a line and a terminal's output impossible
to render as it was written.

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
| segment being written, in the page cache | seconds to minutes | an inotify event and a memory read | live attach, fan-out to readers |
| node segment files | the retention window | milliseconds | scrollback, replay, search |
| offload target | sealed segments, and beyond the window | seconds | survives the node, long-term evidence |

> Corrected 2026-10-14, when the console was built. The first tier was a ring buffer of the
> agent's, sub-millisecond. It is the segment still being written, in the page cache, which the
> reader reads like any other, woken by inotify on the stream's directory the moment the writer
> appends. A ring buffer would have to be filled by the writer, the one process that must never
> stop draining the workload, and served to readers by it or copied out of it: a second consumer,
> and the blocking risk §5 exists to rule out. Reading the page cache gives the same memory-speed
> reads and the same fan-out with nothing added to the writer. What it costs: a record reaches a
> follower after an inotify event and a read of the file rather than from memory already in the
> reader's hands, and a node out of inotify watches falls back to looking every second.

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

**Offload, as built, 2026-10-14** (Finding 43 of `23-walkthroughs.md`). "Wherever an offload
target exists" named nothing that says whether one does. An offload target is now a repository
that takes streams (§5.3 of `06-storage.md`): a workload's stream goes to its project's, or else
to its organization's whole, and there is at most one of each. The first repository made for an
organization or project takes its streams unless told not to, and a second may not, so offload is
on by default once anywhere to keep it exists, and which repository holds a stream is never a
choice made per workload. The node's agent follows each stream's changes log from a place of its
own, kept on disk, and sends every segment sealed there that it still holds: it asks the control
plane for the workload's data key and an hour's write access to the workload's stream prefix, given
to a node that held the workload's lease (as a segment record is believed), seals the segment
again under a key derived from the data key for segments, writes it named by its first offset and
digest, and journals it as offloaded, which marks it so in the index. It is a task of its own, so
the reconciler never waits on a store. A segment sealed while no repository took the stream is
sent once one does, if the node still holds it; one pruned first was held only on the node.

What it costs: an hour of write access to a stream prefix in the control plane's access record
for each stream that seals, and each segment written twice, once on the node and once to the
target.

**Reading an offloaded stream, as built, 2026-10-14.** The index records which repository holds
each offloaded segment, as the grant the node offloaded it with named it, and believes that only
from a node the control plane gave write access to the workload's streams there, which the access
record now keeps with the node it was given to. `StreamService.ReadOffloaded`, under
`workload.console.read`, reads them back without the node: the control plane records the read as a
sensitive one (§4 of `15-observability.md`), takes the workload's key and fifteen minutes of read
access to its stream prefix in each repository holding a segment, fetches each segment, and sends
its records in offset order only once the segment's own digest and the index's agree, so a segment
the node or the store replaced is refused (`stream.segment_altered`) rather than shown. Offsets
between offloaded segments are sent as missing; those after the last are not offloaded yet, and the
last chunk says where that is. This is how a retired workload's stream is read (question 6), and
`korpis logs --offloaded` is its command (§8 of `13-surface-cli.md`).

What it costs: the control plane handles the records of a tenant's console, which a read through a
node never puts in its hands; that is why the read is recorded, and why it reads only with the
operator's custody of the key (§5.5 of `06-storage.md`). What it does not do: search, or follow;
those stay on the node, where the records are written. Nor does the control plane check that a
segment journaled as offloaded is in the target until someone reads it: a node given access could
say it sent a segment it did not, and the read then reports it altered rather than missing.

**How long an offloaded segment is kept, as built, 2026-10-14.** The node's retention (§5) bounds
what its quota pays for; it is not how long evidence should last, and offload exists for the
longer. A workload declares `offload_retention` with its stream's limits, thirty days when it says
nothing, and an offloaded segment is kept that long from when the control plane heard it was
offloaded, by the control plane's clock, since a segment's own times are the node's and
informational (§2). The operator's retention floor raises it where it is lower, and every tenant
is shown both with the stream (§8 of `17-security.md`). The declared retention is kept on the
stream and refreshed from the intent its workload runs at each segment the node reports, so a
retired workload keeps the last it declared, and one query finds every segment past its own. An
hourly pass of the control plane deletes those with the target's own credential, beneath the
workload's stream prefix only, and forgets each in the index only once its object is gone, so a
failure is tried again by the next pass rather than leaving an object nothing names; each deletion
is recorded as `stream.offload_expired`. A segment its node still holds stays readable there.

What it costs: the floor applies to offloaded segments and not to the node's retention, which a
floor would have to enlarge at the tenant's quota; evidence the operator wants kept is kept off
the node, where it survives the node anyway. What it does not do: a reader that listed a segment
just before the pass deleted it is told the segment was altered, not that it expired; and a
segment past its retention and no longer on its node is counted among the offsets lost, which they
are, without saying that retention, not a failure, lost them.

**The control plane holds the index of segments, never their bytes.** Each seal and each offload
is a record in the agent's journal (§4.10 of `02-architecture.md`), ingested like any other, so
the control plane knows every segment's offsets, digest, and where it is held: the `segments` of
the `Stream` in §3.6 of `01-model.md`. That index is what answers the question offload exists
for. When a node is gone, its offloaded segments are found from the index rather than by listing
the offload target, and a reader is told which offsets were held only on the node and are lost,
as a gap (§3). What it costs is two journal records per segment, one at its seal and one at its
offload, so the index grows with the number of segments and never with the number of records.
The index arrives with streams in Phase 1, and is in
the schema from Phase 0 by rule E of `20-roadmap.md`. A client reads it with `StreamService`: the
segments, the bytes held only on the node, the offsets lost, and the retention the stream is kept
to, under `workload.console.read`, because how much a workload wrote, and when, is part of what it
wrote. Before its first seal a stream is reported with no segments and no identifier yet, since
the index first hears of it then; a workload whose console mode is `none` has none, and is not
found.

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

**As built** (Phase 1, the `oci` driver), the node segment files are written by
`korpis-stream-writer`, which the driver names as the task's logging binary in containerd's
`binary://` form. containerd's shim starts it with every task, hands it the workload's stdout and
stderr, and starts the task only once the writer says it is ready, so no output precedes it. Being
the shim's child, the writer outlives a restart of the agent or the driver exactly as the workload
does (§4.4 of `02-architecture.md`). The stream's limits are labels on the container, so a task
started after a driver restart is told the same; the secret values it redacts (§7) are not labels,
which anyone who can read containerd's records would see, but a file in the driver's own state
directory, readable by the driver's user alone, whose path the writer is given. When the task is
deleted the shim closes the pipes and sends the writer SIGTERM, killing it twelve seconds later (§5
of `research/evidence.md`); the writer drains for at most three seconds, seals, and exits, so a
workload's last output is a sealed segment rather than a file left open for a writer that never
comes.

The stream's directory is a volume of its own, `<workload>.stream_console`, that the agent asks
the privileged helper for before Create, sized to the most the stream can hold (§5), so the quota
on it is the kernel's (K-3). The `_` in its name is a character no declared volume's name may
hold, so it never meets one, and the driver refuses a stream directory that is not the workload's
own stream volume under the helper's root. The writer records each seal and prune as a line of a
`changes` log in that directory, durable before it goes on. The agent reads the log on every pass
of its reconciler from the byte it last journaled, journals each change as a segment record, and
keeps the new position only once the journal has the record; a change journaled twice is the same
segment to the index. The control plane believes a segment record from any node that has held the
workload's lease, released or not, because a segment is sealed as the container goes and pruned
long after: the node that wrote a stream is the one that ran the workload.

**Retiring a workload keeps its stream, as it keeps its volumes** (§3.6 of `01-model.md`: streams
outlive the workload, and deleting a volume is a separate, irreversible plan step). The agent reads
the changes log once more after the workload leaves its set and then stops watching it; the
segments stay where they are and in the index. A crashed and retired workload's last output is
there to be read, which is the invariant's point. Deleting a workload's stream is the same step
that deletes its volumes: the deletion names `stream_console` (§5 of `13-surface-cli.md`), and the
plan deletes that volume by its own irreversible step after the runtime is removed (§7 of
`06-storage.md`). Before the agent deletes it, it reads the changes log through and journals what
is left, so the index hears of the last segment before it hears the volume is gone; the control
plane then holds that no segment of the stream is on the node, and every offset not offloaded is
reported lost. Its offloaded segments go too, at the next hourly pass, kept no longer than the
operator's disclosed floor (§8 of `17-security.md`): deleting a stream that left copies behind
would not delete it. What ends the stream is the approved plan's step deleting its volume, as the
control plane records the step done, never the node's report that the volume is gone, so a node
cannot have a stream's offloaded evidence deleted by saying it deleted the stream.

What it costs. The writer is the only reader of the workload's pipes, so if it dies the
workload's next write to its console fails with `EPIPE`, or kills it with `SIGPIPE` if it does not
ignore that signal. The writer is built never to exit on a write error, and it records what it
cannot write as a gap (§5) rather than stopping; a writer killed by the kernel's out-of-memory
killer still takes its workload's console with it. A retired workload's stream holds its disk
until it is deleted, and it no longer counts against a quota once the workload stops counting,
the same gap retained volumes have. And a driver other than `oci` writes no stream until it
captures one the same way.

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

**As built** (`crates/korpis-stream`), the default is 256 KiB a second with a burst of 4 MiB,
admitted by a token bucket in bytes. A record that finds the bucket empty is dropped and counted;
the next record admitted, or a second after the last drop if none is, writes one gap record with
the records and bytes lost, the time they were lost over, and the reason. Segments are sealed at
8 MiB or five minutes, whichever comes first, and the sealed ones are pruned oldest first to the
stream's retention, 64 MiB by default, or by age where the workload declares a duration instead.
The limits are the intent's `streams` (§3.3 of `01-model.md`), and the most a stream can hold
counts against the workload's disk quota as a volume it creates would (K-3): its retention, the
segment still being written, and for a retention by age all its rate can write in that time.
That is 72 MiB for the defaults; a workload whose console mode is `none` holds nothing. Records
reach the page cache as they are written and the disk at least once a second and at every seal:
the writer's crash loses nothing, and the machine's loses at most the last second, which the next
start finds as a torn frame and cuts away.

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

**As built** (Phase 1), input is a data-path session of `workload.console.write`, each frame a
`ConsoleInput`: bytes for the workload, or a new terminal size. The node journals each input as
an effect before it does anything else with it: the session's subject sent so many bytes on that
session, under the grant the session's token was minted from. Only then does it hand the bytes to
the driver, so an input the workload received is never one the journal lacks. The `oci` driver
runs an `interactive` console under a terminal and sends input to the stream writer, which records
it in the stream as an input record naming its sender, redacted like every record (§7), and then
feeds the workload's stdin, which it holds open for the task's whole life (§4.2 of
`04-runtimes.md`). A resize is recorded as a control record and applied to the terminal. The
stream says what was typed and the effect log says who typed it under which authority, which is
the row the paragraph above promises, with the secret a person types at a prompt kept out of the
effect log, which holds only how many bytes were sent.

**The control plane does not take the node's word for whose input it was.** It records the
effect as the act of the subject the node names only if the grant named is that subject's and, as
issued, permitted `workload.console.write` on the workload when the node says the input happened;
otherwise the record is kept as a rejection. So a node can attribute input only to someone who
could have given it. What that check leaves to the components that judged it at the time:
revocation, which the node judged against the list it held (§6.3 of `08-identity.md`); the
client's network, which the relay judged; and a second factor, which the gateway judged when it
minted the token. What it costs, and what it does not do: a compromised node that carried a
subject's session can still attribute input to that subject that the subject never typed, during
or after the session, because nothing the client sends is signed by the client; the check bounds
whom a node can implicate, not what it can claim they did. Input that finds the workload not
reading its stdin is dropped and recorded as dropped, so the writer never stops draining the
workload's output to wait for it.

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

Redaction runs before the size limit of §9's question 5, never after: cutting a record first could
keep half of a secret, while cutting a redacted record can only shorten its mark. A value shorter
than four bytes is not matched at all, because matching it would redact every occurrence of those
letters in everything the workload prints; the field is reported as not covered instead.

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

**As built** (Phase 1), reading is a data-path session of the action `workload.console.read`,
opened at a relay with a capability token and served by the node from the segment files it holds,
the one still being written included (§3.1 of `02-architecture.md`; the messages are in
`console.proto`). A reader asks for an offset, or a node-clock time, which the node turns into the
first offset written at or after it, skipping sealed segments by their footers alone; it may ask to
follow, for a number of records, and for a search. The node answers in chunks, each ending with the
offset to ask for after a reconnection, and says once when it has caught up with what was held
when the session opened. Offsets asked for that it no longer holds are reported as missing, at the
start and again if a slow reader falls behind the retention, never skipped in silence. A search is
a substring or a regular expression, run on the node so a one-line answer does not cross the
network as a whole retention window. The pattern comes from whoever holds the action and the node
runs it, so it is bounded: at most 1 KiB, a compiled form of at most 1 MiB, and an engine with no
backtracking, whose time is linear in the stream whatever the pattern (§5 of
`research/evidence.md`). Gap and control records are sent whatever the pattern, because a search
has to say where it could not look.

What it costs, and what it does not do. A follower reads again when inotify says the stream's
directory changed, and at least every second whatever it hears, and each read lists the directory;
a push from the writer would save that, and would put a second consumer on the one process that
must never stop draining the workload (§4). The node reads only what it holds: until offload is
built (Phase 1, with backup), a segment pruned from the node is gone, and the reader is told so.
And a workload the node no longer holds, a retired one included, is refused on the data path, so a
retired workload's kept stream (§4) cannot yet be read at all (§9, question 6).

---

## 9. Open questions

1. **Offload ownership. Resolved here, by the backup mechanism.** Whether the agent uploads to the
   offload target directly (fast, but the node holds object-storage credentials) or via a broker
   (slower, no node-held credential) is the same trade §7 of `02-architecture.md` resolved for
   registry pulls with short-lived scoped tokens.

It is resolved exactly as backup is (§5.5 of `06-storage.md`): the node receives, per offload job,
a short-lived credential that can write, read, and list objects under that workload's stream
prefix, delete none, and reach nothing else, and segments are encrypted with the workload's data
key before they leave the node. A target that cannot issue such a credential is written through a
relay, which holds the credential instead. The node never holds a credential that reaches another
workload's data.
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
   kill. **Resolved here, 2026-10-14:** a record holds at most 64 KiB. The first 64 KiB of a longer
   line are kept as its record, the rest is dropped as it arrives, never buffered, and a gap record
   counts the bytes dropped. 64 KiB holds any line a person reads and every stack frame a program
   prints, and bounds what one line can cost the writer.
6. **Reading a retired workload's stream.** Retiring keeps the stream (§4), and the data path
   refuses a session for a workload the node no longer holds, so what is kept cannot be read: the
   final output §3.6 of `01-model.md` promises is there, and no session reaches it. Either the node
   serves a stream it still holds for a workload it no longer runs, judged by a token the gateway
   mints for a retired workload, or the stream is read from the offload target. Settled with
   offload, which has to answer the same question for a node that is gone. → here
   Resolved in §4 here: the stream is read from the repository that holds its offloaded segments,
   through the control plane, which needs no node and no token for a workload that no longer runs.
   What the node held and never offloaded, because no repository took the stream, is not read.
