# Streams

A workload's console and logs as durable, seekable records: how they are written, kept, offloaded,
read, redacted, shared and audited. In Pterodactyl the console is a websocket into a memory buffer:
reload the page and it is gone; ask what a server printed before it crashed and the answer is a file
on a node; ask who typed `stop` and there is no answer. A console is not a widget but a record:
support reconstructs incidents from it, moderation adjudicates from it, and in a multi-tenant system
it is the primary evidence of what a tenant's software did.

## Contents

- [Records](#records)
- [Gaps are records](#gaps-are-records)
- [Where a stream lives](#where-a-stream-lives)
- [The writer](#the-writer)
- [The workload is never blocked](#the-workload-is-never-blocked)
- [The index](#the-index)
- [Offload](#offload)
- [Runs' streams](#runs-streams)
- [Retiring and migrating](#retiring-and-migrating)
- [Reading](#reading)
- [Input is an effect](#input-is-an-effect)
- [Redaction before persistence](#redaction-before-persistence)
- [Sharing and replay](#sharing-and-replay)

## Records

```
Record
  stream_id, offset     monotonic per stream: the seek primitive
  time                  the node's clock at write, informational
  source                stdout | stderr | file:<path> | korpis | input
  kind                  text | structured | gap | control
  body                  bytes
```

Offsets, not timestamps, are identity, because node clocks step and a reader seeking by time cannot
say "resume exactly where I stopped"; a reconnecting client sends its last offset and receives what
follows. A text record is the bytes as written, line end included, and a line still unfinished when
flushed (a prompt waiting for input) is a record of its own with no line end, so a reader reproduces
output exactly. `structured` keeps JSON lines a workload already emits intact for export. `control`
carries terminal events (resize, signal, EOF), since a terminal that cannot be resized renders game
server menus wrong. A record holds at most 64 KiB: the first 64 KiB of a longer line are kept, the
rest dropped as it arrives, never buffered, and counted by a gap record.

## Gaps are records

A gap record carries how many records and bytes were lost, over what time, and why. Every log system
drops under pressure and almost none say so, and someone then reads a log with an invisible hole and
concludes from the silence that nothing happened. A log that lies by omission is worse than none,
because it is trusted. The marker has an offset, persists, appears in exports, renders in every
client, and survives replay.

## Where a stream lives

| Tier | Holds | Latency | Purpose |
|---|---|---|---|
| the segment being written, in the page cache | seconds to minutes | an inotify event and a read | live attach, fan-out |
| node segment files | the retention window | milliseconds | scrollback, replay, search |
| an offload repository | sealed segments, beyond the window | seconds | survives the node, long-term evidence |

The live tier is the page cache, not a ring buffer of the agent's: a ring buffer would have to be
filled by the writer, the one process that must never stop draining the workload, and served by it
or copied out, a second consumer and a blocking risk. A follower is woken by inotify on the stream's
directory, or every second where the node is out of watches.

The agent owns all three, and the control plane is not in the data path: a hundred consoles on ten
nodes is ten agents fanning out locally, joined to readers by stateless [relays](relays.md). A
reader holds a short-lived capability token naming the workload, which relay and agent both verify.

## The writer

The node's segment files are written by `korpis-stream-writer`, which the `oci` driver names as the
task's logging binary in containerd's `binary://` form: the shim starts it with every task, hands it
stdout and stderr, and starts the task only once it is ready, so no output precedes it; being the
shim's child, it outlives an agent or driver restart as the workload does. The stream's limits are
container labels; the secret values it redacts are a file in the driver's own state readable by the
driver's user alone, not labels anyone reading containerd's records would see. When the task is
deleted the shim closes the pipes and sends the writer `SIGTERM`, killing it twelve seconds later,
and the writer drains for at most three seconds, seals and exits, so a workload's last output is a
sealed segment. The shim holds the pipes' write ends until then (containerd's
`cmd/containerd-shim-runc-v2/process/io.go`, `binaryIO.Close`), so a task that stopped but was kept
leaves its last segment open. So the driver deletes a task once a `Stop` or `Kill` has stopped it,
or a `Stop` finds it stopped, first recording its exit status and time in the container's label
`korpis.ended`, which `Inspect` reads while the container has no task. The agent stops a workload by
signal and then kill, so once what it halted has stopped it asks `Stop` of it, and at a migration's
`quiesce` it asks again while the stream is open, for a workload that had exited by itself: a
stopped workload's stream is complete, which `quiesce` waits for. A task that exits on its own is
otherwise kept until the agent stops it or starts it again. A guest has no shim, so its driver starts the writer itself (see
[guests](guests.md#consoles)).

The stream's directory is a volume of its own, `<workload>.stream_console`, which the agent asks the
helper for before `Create`, sized to the most the stream can hold, so its quota is the kernel's; `_`
cannot appear in a declared volume's name, and the driver refuses a stream directory that is not the
workload's own. The writer records each seal and prune as a line of a `changes` log there, durable
before it goes on; the agent reads it on every pass from the byte it last journaled and journals
each change as a segment record.

The writer is the only reader of the workload's pipes, so if it dies the workload's next console
write fails with `EPIPE`, or kills it with `SIGPIPE`. It never exits on a write error, recording
what it cannot write as a gap; a writer killed by the out-of-memory killer still takes its
workload's console with it.

## The workload is never blocked

When a workload writes faster than the stream persists, blocking would let a logging burst freeze a
game server, and spilling without bound converts a workload's bug into a node-wide outage. So Korpis
drops and emits a gap. The rate is declared, with a default, and a workload producing high volume
declares a higher limit and pays from its own quota.

The default is 256 KiB a second with a burst of 4 MiB, admitted by a token bucket in bytes; a
dropped record is counted, and the next admitted record, or a second after the last drop, writes one
gap record. Segments seal at 8 MiB or five minutes, and sealed ones are pruned oldest first to the
retention, 64 MiB by default, or by age where declared. The limits are the intent's `streams`, and
the most a stream can hold counts against the workload's disk quota as a volume would: its
retention, the open segment, and for retention by age all its rate can write in that time; 72 MiB
for the defaults. Records reach the page cache as written and the disk at least once a second and at
every seal, so a writer crash loses nothing and a machine crash at most the last second, which the
next start finds as a torn frame and cuts away.

## The index

The control plane holds the index of segments, never their bytes. Each seal and offload is a journal
record, ingested like any other, so it knows every segment's offsets, digest and where it is held.
When a node is gone its offloaded segments are found from the index, not by listing the repository,
and a reader is told which offsets were held only on the node and are lost, as a gap. The index
grows with segments, never records. `StreamService` reads it (segments, bytes held only on the node,
offsets lost, retention) under `workload.console.read`, since how much a workload wrote, and when,
is part of what it wrote. The control plane believes a segment record from any node that ever held
the workload's lease, since a segment is sealed as the container goes and pruned long after.

## Offload

**Offload is on wherever a repository takes streams, and segments are offloaded as they seal**, so a
dead node takes only the segment being written. A workload's stream goes to its project's streams
repository, else its organization's; there is at most one of each, the first repository made for an
organization or project takes streams unless told not to, so which repository holds a stream is
never a choice per workload. Where none exists, the interface says how much of each stream exists
only on its node.

The agent follows each stream's changes log from a place kept on disk and sends every sealed segment
it still holds, as a task of its own so the reconciler never waits on a store: it asks for the
workload's data key and an hour's write access to the workload's stream prefix (given to a node that
held the lease, and recorded with the node it was given to), seals the segment again under a key
derived for segments, writes it named by first offset and digest, and journals it offloaded. A
segment sealed before any repository took the stream is sent once one does, if still held. The node
never holds a credential reaching another workload's data.

**Reading an offloaded stream.** `StreamService.ReadOffloaded`, under `workload.console.read`, reads
without the node: the control plane records a sensitive read, takes the key and fifteen minutes of
read access in each repository holding a segment, and sends records in offset order only once a
segment's own digest and the index's agree, refusing one the node or store replaced
(`stream.segment_altered`). Offsets between offloaded segments are sent missing. This is how a
retired workload's stream is read (`korpis logs --offloaded`). The control plane then handles a
tenant's console, one reason the read is recorded and needs operator custody of the key; it does not
search or follow, which stay on the node, and does not check a segment journaled offloaded is in the
repository until read.

**How long.** The node's retention bounds what its quota pays for; it is not how long evidence
should last. A workload declares `offload_retention`, thirty days by default, kept from when the
control plane heard of the offload, by its clock; the operator's disclosed retention floor raises
it, and every tenant is shown both (see [security](security.md)). The declared retention is
refreshed from the running intent at each segment, so a retired workload keeps its last. An hourly
pass deletes expired segments with the repository's own credential beneath the workload's stream
prefix, forgetting each only once its object is gone, recorded as `stream.offload_expired`.

## Runs' streams

A `task` or `scheduled` workload writes no console stream of its own: each [run](runs.md) writes
one, its `run` set and offsets from zero, so Tuesday's failure is read by naming Tuesday's run. Each
has the declared limits and retention, and goes with the run when retention deletes it, node
segments and offloaded ones. Its offloaded segments go beneath `segments/runs/<run>/` of the
workload's stream prefix, the run bound into the sealed name, so a segment opens only as its run. A
run's segment is believed only for a run the reporting node started. A run's stream is watched while
its workload is held, since a finished run's console is what a reader names it for, and is read by
naming the run, beneath the session's workload. The node deletes a run's stream volume only once the
run is absent from the assignment's list of every run the control plane still holds and the node no
longer holds it, so a run just stopped is not deleted before the control plane hears it ended. A
stream directory per kept run counts against disk quota, bounded by `keep_successful`, `keep_failed`
and `max_active`.

## Retiring and migrating

**Retiring keeps the stream**, as it keeps volumes: a crashed and retired workload's last output is
there. The agent reads the changes log once more and stops watching. Deleting the stream is the
deletion naming `stream_console`, by its own irreversible step; before deleting the volume the agent
journals what is left, so the index hears of the last segment first, and every offset not offloaded
is then lost. Offloaded segments go at the next hourly pass, kept no longer than the operator's
floor. What ends a stream is the approved plan's step as the control plane records it done, never a
node's report that the volume is gone, so a node cannot have a stream's offloaded evidence deleted
by saying it deleted the stream.

**A stream migrates with its workload**, and the destination continues the offset sequence rather
than restarting it, or every reader's stored position would silently mean something else across the
event most likely to make someone go looking. The stream's directory is a volume and crosses as its
class's volumes do. At `quiesce` the source's writer seals and exits, and the source reports each
stream's next offset and the changes-log position its agent journaled and offloaded to; at `cutover`
the destination's writer starts at the larger of the offset it finds and the one handed over,
writing a gap between them where less arrived, and its agent follows from the handed position, so
nothing is journaled or offloaded twice. **What evaluates "this offset continues the sequence":**
the destination's writer, from the watermark in its assignment, taken from the source's report.

## Reading

Reading is a data-path session of `workload.console.read` (messages in `console.proto`), served by
the node from its segment files, the open one included. A reader asks for an offset, or a node-clock
time turned into the first offset at or after it (sealed segments skipped by their footers); it may
follow, and search. The node answers in chunks, each ending with the offset to resume from, and says
once when it has caught up. Offsets it no longer holds are reported missing, at the start and if a
slow reader falls behind retention, never skipped in silence. A search is a substring or regular
expression run on the node, so a one-line answer does not cross the network as a whole window; the
pattern is bounded (1 KiB, a compiled form of 1 MiB, an engine with no backtracking, linear in the
stream). Gap and control records are sent whatever the pattern, since a search has to say where it
could not look. Search within retention is provided because it is a property of the segment files;
Korpis is not a log search product, and streams export to Loki, OpenTelemetry or object storage.

**A read is an effect before it is anything else**, as input is: once the node finds the stream it
journals `workload.console.read` as the session subject's act, naming session, run, pattern and
whether it follows, and sends nothing until the journal holds it. The control plane believes it only
as far as the token's grant permitted the read then. One effect per session, not per chunk. A node
its operator changed can serve a read and journal nothing.

A follower reads again on inotify and at least every second, listing the directory each time; a push
from the writer would save that and put a second consumer on the process that must never stop
draining. A workload the node no longer holds is refused on the data path and read from its
offloaded segments. A client reading a stream it may not read gets `NOT_FOUND`, not `DENIED`.

## Input is an effect

Who stopped the server is one of the most-asked questions in this market, and incumbents cannot
answer it: their input is a websocket frame with a session cookie and no record. Here it is a row,
for the same reason every authority-bearing action is.

Input is a data-path session of `workload.console.write`, separate from reading, so watching and
typing are delegated apart. Each frame is bytes for the workload or a new terminal size. The node
journals each input as an effect first (the subject sent so many bytes on the session, under the
token's grant), and only then hands it to the driver, so an input the workload received is never one
the journal lacks. The writer records it in the stream as an input record naming its sender,
redacted like every record, then feeds the workload's stdin, held open for the task's life; a resize
is a control record applied to the terminal. So the stream says what was typed and the effect log
who typed it under what authority, with a secret typed at a prompt kept out of the effect log, which
holds only the byte count. Clients show an input record as a notice (`input "stop" by user:...`),
never as its body. Input finding the workload not reading is dropped and recorded as dropped, so the
writer never stops draining output to wait for it.

**The control plane does not take the node's word for whose input it was.** It records the effect as
the named subject's act only if the named grant is that subject's and, as issued, permitted
`workload.console.write` on the workload when the node says the input happened; otherwise it is kept
as a rejection. So a node can attribute input only to someone who could have given it. Revocation,
the client's network and a second factor were judged at the time by node, relay and gateway. A
compromised node that carried a subject's session can still attribute input the subject never typed,
since nothing the client sends is signed by the client; the check bounds whom a node can implicate,
not what it claims they did.

## Redaction before persistence

Game servers print their configuration on startup, RCON passwords, API keys and database URLs among
it, and every panel in this market persists those into logs support staff read. Recipes declare
which fields are secret, and the writer applies their values as literal patterns at write time, on
the node, before anything is persisted, so a secret never reaches a segment, an offload, a backup or
an export:

```
[14:30:02] RCON enabled, password ████████ (redacted: rcon_password)
```

Redaction runs before the record size limit, since cutting first could keep half a secret. A value
shorter than four bytes is not matched, since it would redact every occurrence of those letters, and
the field is reported as not covered. **It is best-effort and labelled so**: a workload
base64-encoding its own secret before printing defeats pattern matching, and nothing fixes that;
what it defeats is the overwhelmingly common case of a program echoing a value verbatim. Operators
may add patterns, and recipe-declared ones cannot be removed by a tenant, who is not always the
party being protected.

## Sharing and replay

A stream range is a narrowing of a grant, so sharing one is a grant:

```
korpis stream share mc-survival --from 14:30 --to 14:45 --expires 7d
-> https://panel.example/g/#korpislk_c22e...
```

The recipient sees exactly that range, with no account, until the link expires, which is what
support teams do today by pasting screenshots into tickets. The range is a condition of a grant,
`stream_range`, beside its expiry and networks, on a grant whose one action is
`workload.console.read` scoped to the workload; it is not a scope, because a stream's identifier is
one the node never learns, and the node verifies a token offline against the workload it serves.
Attenuation keeps a child's range within its parent's. Three readers enforce it: the node's reader
starts at the first record written at the range's start, sends and searches only records within it,
and ends at the first past its end; the control plane's offloaded reader sends only records within
it; and the index shows only segments holding some of it. The node refuses any other action under a
ranged token. The bounds are the node's clock, so a record written after the clock stepped back into
the range is not shown. Each reading spends a link use.

Replay is a seek: the web console, the chat window and `korpis console --at` are views onto the same
offsets, so what a server looked like when it crashed is a query, not a reconstruction. Stack traces
span many records and stdout and stderr interleave; records are presented raw, `structured` being
the path for programs that want events kept whole.
