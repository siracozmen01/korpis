# Health

What a node judges of a running workload, and what the verdicts decide. `kill(pid, 0)` is not a
health check, and for half the tiers it is not even possible:

| Tier | What "the process is alive" tells you |
|---|---|
| `process` | the process exists, nothing about whether it works |
| `container` | the container's init exists, nothing about the application |
| `microvm`, `vm` | the hypervisor runs; the guest may have panicked twenty minutes ago |

So health is application-level, declared by the recipe and judged by the node from outside the
workload. TCAdmin sells service monitoring and game monitoring as different products; they are
different, and neither is a premium feature.

## Contents

- [Probes](#probes)
- [Timing and verdicts](#timing-and-verdicts)
- [What the verdicts decide](#what-the-verdicts-decide)
- [Each kind](#each-kind)
- [What it costs](#what-it-costs)

## Probes

| Probe | Applies to | Mechanism |
|---|---|---|
| `tcp` | any | connect to an endpoint |
| `http` | services | `GET` a path, check the status |
| `query` | consoles | a game or application query protocol, from an [extension](extensions.md) |
| `exec` | drivers declaring `exec` | run a command inside, check its exit status |
| `guest_agent` | drivers declaring `guest_agent` | the driver asks the guest's own agent |
| `heartbeat` | drivers declaring `identity_dir` | the workload calls its identity socket; silence is the signal |
| `exit` | `task`, `scheduled` | exit status |

A node whose drivers cannot run a workload's probe refuses it at [placement](placement.md) (the
`probe` check), rather than accepting a probe nobody runs.

## Timing and verdicts

A probe that does not say is tried every 10 seconds, given 5 seconds and never more than its period,
judged failing after 3 attempts in a row fail, and first tried as its object starts. A period under
a second is taken as one, so no recipe makes the prober a load on what it judges. A probe passes at
its first success and fails only after its `failures`; before either it is unjudged. Each start of
the object, and an object built from another intent, is judged afresh. An attempt the prober could
not make (its driver unreachable, its provider silent) is unjudged, never a failure, since the
prober's failure is not the workload's.

## What the verdicts decide

- **Readiness** decides whether the workload's endpoints are `ready`, and whether a dependent
  requiring it is created (see [discovery](discovery.md)).
- **The health the node reports** is the liveness verdict, or readiness where no liveness probe is
  declared, and `unknown` while unjudged or while the object does not run. A change is the
  `health.changed` event.
- **A failing liveness probe restarts the workload as a crash does**: the same backoff, a second
  doubling to five minutes and forgiven after a minute up, counted with its crashes, recorded as
  `node.restart` with the cause `unhealthy` and the probe's kind. Under an expired
  [lease](node-agent.md), only where the workload keeps running.

## Each kind

**`tcp` and `http`.** Each attempt is made from a socket the helper opens inside the workload's
network namespace (`OpenProbe`), connected to the workload's address and the named endpoint's port.
From inside, it reaches the endpoint as the workload's own loopback does (for a microVM, as its tap
does), so an endpoint whose rules turn away the node's address, or whose exposure is `none`, is
probed all the same, and the socket reaches nothing the workload could not. `tcp` passes when
connected; `http` sends an HTTP/1.0 `GET` and passes on the expected status, or any 2xx. Each
attempt, connecting included, is held to the timeout.

**`query`** is an exchange the node runs and its protocol's health provider directs: from a socket
of the endpoint's transport, opened as above, the node asks the control plane what to send, sends
it, reads until it has what it was told to read, the workload closes, or the round's wait runs out
(over UDP, the first datagram back), and asks again with the round until the provider's verdict. The
probe's timeout bounds the workload's share. A question the control plane does not answer within ten
seconds, or answers as unjudged, leaves the attempt unjudged, since a provider that fails says
nothing about the workload. A query probe runs at most every five seconds, each round being a call
through the control plane's queue, and never twice at once.

**`heartbeat`** is the workload's own word: a process in it calls `Heartbeat` on its identity socket
(the SDK's `IdentitySocket::heartbeat`, or `korpis heartbeat --every 30s`), and the node keeps when
it last heard, nothing more. A beat asks nothing of the control plane, so a workload may beat as
often as it likes and a node cut off still judges it. On the probe's period the check passes when a
beat came within `max_interval` in this life of the object, fails when none has, and judges nothing
while the life is younger than its initial delay and `max_interval`. A beat from before the object
last started does not count, so a workload restarted for silence is judged on its new life. A
heartbeat says the calling process runs, not that the workload works; a workload beating from a
thread of its own reports healthy while the rest is stuck, which is why a probe from outside is
preferred where there is an endpoint.

**`exec`** runs its command through the driver's `Exec` (see [runtimes](runtimes.md#lifecycle)) in a
task of its own, so a slow command holds nothing up: exit zero passes, anything else or a kill for
the timeout fails. The command runs in the workload's cgroup and counts against its limits, so a
workload at its process limit cannot fork it and is judged failing, which is what such a workload
is. The node's log keeps the first 4 KiB of a failing command's output.

**`guest_agent`** asks the driver's `PingGuest`, which asks `korpis-guest` on the guest's control
port (see [guests](guests.md#the-control-port)). An answer passes and silence fails: a guest whose
agent does not answer has a kernel or agent stopped while its hypervisor runs, which the tier's own
signal cannot show. The node asks; the host listens for nothing a guest could open.

## What it costs

One connection a period for each probe of each running workload, and a helper call for each socket.
A workload whose liveness probe fails a minute after each start is restarted every minute and never
backs off further, as one crashing a minute after each start is. Where a node gives workloads no
network it runs no socket probe, and where it serves no identity sockets, no heartbeat.
