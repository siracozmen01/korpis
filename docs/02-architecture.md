# Architecture and the Reconciliation Protocol

**Status:** design **Date:** 2026-08-07 **Depends on:** [`00-overview.md`](./00-overview.md),
[`01-model.md`](./01-model.md)

---

## 1. Why this document matters more than the others

Almost everything in Korpis can be changed later. Storage engines can be swapped, UIs rewritten,
schedulers replaced, drivers added. One thing cannot: **the protocol between the control plane and
the data plane.** Once agents are deployed on machines Korpis does not control, that contract is
frozen for as long as those machines exist.

Pterodactyl's Panel and Wings are coupled by version rather than by contract. Issue #141, "support
for third-party daemons", was opened in October 2016 and answered the same day: it was already
possible "so long as the daemon you are using follows the same API style as the core daemon".
Closing it three days later, the maintainer added that supporting daemons from external authors is
difficult "unless said daemon is specifically designed to work with Pterodactyl". That answer is
the finding. The contract was whatever the reference daemon happened to do, so the only way to
conform was to imitate an implementation. It was not neglect; it is what happens when a protocol is
never designed as a protocol.

> Second external review, finding S56. Recorded in §17 of `23-walkthroughs.md`. An earlier version
> of this paragraph said the issue had been open, unanswered, since 2016. It was answered and
> closed within three days, and the answer supports the argument better than the claim did.

Rule K-7 requires that a third party be able to write a conforming agent without reading Korpis'
source. This document is what makes that possible.

---

## 2. The split

```
┌─────────────────────────────────────────────────────────────────┐
│  CLIENTS                                                        │
│  web · Discord · CLI · API consumers · extensions               │
│  Peers. None privileged. All speak the same API. (P2)           │
└───────────────────────────┬─────────────────────────────────────┘
                            │
┌───────────────────────────▼─────────────────────────────────────┐
│  CONTROL PLANE                                                  │
│                                                                 │
│  Owns: Organization, Project, Quota, Grant, Workload,           │
│        Intent, Plan, Recipe refs, Placement, Lease              │
│  Reads: Observation, Effect                                     │
│                                                                 │
│  Decides. Never commands.                                       │
└───────────────────────────┬─────────────────────────────────────┘
                            │  one long-lived stream per node,
                            │  dialled OUT by the agent
┌───────────────────────────▼─────────────────────────────────────┐
│  DATA PLANE, one agent per node                                │
│                                                                 │
│  Owns: Observation, runtime objects, local content store,       │
│        volumes, network namespaces, stream segments             │
│  Reads: Intent, Lease                                           │
│                                                                 │
│  Converges. Never waits to be told.                             │
└─────────────────────────────────────────────────────────────────┘
```

**The ownership rule is absolute.** Every object in `01-model.md` has exactly one writer. The
control plane never writes an `Observation`; the agent never writes an `Intent`. There is no object
both may modify. This is what makes "the panel says stopped but the server is running" structurally
impossible rather than merely unlikely (P1).

---

## 3. Connection direction: the agent dials out

**The agent opens its connections outward: one long-lived control stream to the control plane, and
one data connection to a relay (§3.1). Nothing ever connects to a node.**

Pterodactyl works the other way: the Panel makes HTTP calls to Wings, so every node needs a
reachable address, an open inbound port, and a certificate. That requirement is invisible in a
datacentre and crippling everywhere else.

Dialling out gives, all from one decision:

- **Nodes need no inbound ports.** No public IP, no port forwarding, no inbound firewall rule.
- **NAT and residential connections work.** A home machine, a machine behind CGNAT, a machine in
  someone else's cluster, all are ordinary nodes.
- **The firewall story is one sentence:** the node must be able to reach the control plane and its
  relays on 443. On one machine they are the same process and the same port.
- **Authentication is one-directional.** The agent proves who it is on connect. The control plane
  never needs credentials for the node.
- **Decisions and data travel separately, and both travel outward.** Assignments, observations,
  leases, and journal records share the control stream. Console bytes, file transfers, and stream
  segments share the data connection. Separating them is what keeps a bulk transfer from delaying a
  lease renewal, and dialling both outward is what keeps the node without a listening socket.

The cost is that the control plane and the relays hold many long-lived connections. That is a
well-understood engineering problem with well-understood solutions, and it is a far better problem
than requiring every operator to expose every node to the internet.

### 3.1 The data path: relays

> Second external review, finding S19. Recorded in §17 of `23-walkthroughs.md`.

Four documents sent clients **directly to the node**: web uploads (§8 of `11-surface-web.md`),
`korpis files sync` (§8 of `13-surface-cli.md`), stream readers (§4 of `14-streams.md`), and live
console keystrokes (§6 of `08-identity.md`). Under §3 a node has no inbound port and may sit behind
CGNAT, so a browser cannot reach it, cannot validate a certificate for it, and the design had no
component that could. The contradiction was real, and either half could have been given up. Giving
up §3 would make every residential and NAT-bound node second-class. Giving up the direct path by
routing bulk data through the control plane would make a 40 GB world upload a control-plane load
problem and make console availability depend on the database. Neither is acceptable, so the data
path gets its own tier.

**A relay is a stateless process that joins a client connection to a node's outbound data
connection.** It has no database, holds no grants, and evaluates no policy beyond verifying a
capability token.

```
client ──TLS──▶ relay ◀──TLS, dialled out── agent
          │                                    │
          │ capability token, verified offline │ the same token, verified again
          ▼                                    ▼
   routes the session to the node's data connection; never buffers it to disk
```

- **Every session is authorized twice, by the same token.** The gateway evaluates the grant chain
  online and issues a short-lived capability token naming one workload and one action (§6 of
  `08-identity.md`). The relay verifies its signature, expiry, and the revocation list before
  routing. The agent verifies it again before acting, so a compromised relay can observe sessions
  it carries but cannot invent one. (Corrected 2026-10-22, with SFTP, below: a relay that serves
  SFTP authenticates its clients' keys itself and asks the gateway for their tokens, so it can
  invent a file session as anyone who registered a key. A relay that does not serve it still
  cannot invent any session.)
- **The relay is the first hop that sees the client's address**, so conditions that depend on it
  (`source_cidr`, §3.2 of `01-model.md`) are evaluated there, or at the gateway for API calls. An
  agent behind a relay never sees a client address and never evaluates one. Behind a load balancer
  the relay accepts the PROXY protocol from configured addresses only, because a header anyone can
  send is not an address.
- **Relays scale horizontally and fail independently.** A node keeps one data connection to each
  of a small configured set of relays, so losing a relay re-routes new sessions rather than losing
  the node. In-flight sessions on a failed relay end; streams resume from their last offset (§2 of
  `14-streams.md`) and file transfers resume from their last acknowledged chunk.
- **Console survives a control-plane outage for exactly one token lifetime.** The relay and the
  agent verify offline against the signing key and the last revocation list they received, so an
  established session continues until its token needs renewal, which requires the gateway. This is
  the bounded behaviour §6 of `08-identity.md` promised; before relays existed, nothing could
  deliver it.
- **On one machine the relay is a role of `korpis-server`,** enabled in the same process, speaking
  the same protocol. There is no separate thing to install until there is a reason to scale it.

**As built, on one machine.** The relay role is two services on the control plane's listener. A
node attaches with `DataService.Attach`, proving its key over a `DataHello`, which signs a message
of its own form (`korpis.agent.v1.DataHello|…`), so a relay that sees one cannot present it as a
control-stream Hello, and whose nonce joins the Hello's memory, so neither is replayed as itself.
The message ends with the URL the node dialled, so a hello made for one relay is good at no other
(below). A client opens a session with `RelayService.Open`, naming the token, the action, the
workload, and the route `MintToken` returned with the token: the node the workload is placed on,
sealed by the gateway with a key derived from the token key, under a fresh nonce, so the client
cannot read it and two routes to one node do not compare equal. The relay opens the route with the
same key, which its feed carries beside the token keys, to learn which node to ask; a client that
may read the node can name it instead. The relay verifies the token offline, against
the token keys, the revocation list (§6.3 of `08-identity.md`), and the client's address, and only
a token scoped to the workload itself verifies, because a relay holds no ancestry (§6 of
`08-identity.md`). It then asks the node to open the session under a random 128-bit identifier,
good for one carry and told to that node alone, and the node carries it on a stream of its own
(`DataService.Carry`), so one slow session never holds up another: each stream has its own flow
control, and a relay buffers at most a few frames of either side before the faster side waits. A
wrong node cannot misdirect a session, because the node verifies that it holds the workload; it
refuses, and the client is told the node's reason (`not_here`, `refused`, `unimplemented`). A node
that does not answer within ten seconds fails the open as unavailable. The agent keeps one data
connection to each relay the last `Relays` message named, and to the control plane's own address
when that message says the control plane serves the role. It judges every session again from what
the client sent: the token offline, with the token keys enrollment returned and the revocation list
it holds, and never the client's address, which it does not see; the workload against the set it
holds; and the action against the handlers it runs, so an action with no handler on the node is
refused rather than accepted and left idle. A session ends when its handler returns, with the
handler's reason delivered to the client before either stream closes, and the node ends it itself
when its token expires unrenewed, when a revocation list names its chain, or when the relay refuses
a renewal the client sent on the session (§6 of `08-identity.md`).

> Corrected 2026-10-14, when the first command to open a session was built (Finding 29 of
> `23-walkthroughs.md`). The client named "the node the API reports it placed on", and the API
> reports a placement's node only to a caller who may read the node, which a tenant of a host,
> or a moderator holding a console grant, may not. The relay routes by the node and holds no
> database to look it up, so such a client could open no session at all. The route is the
> answer that keeps every property: the gateway already knows the placement when it mints, the
> relay stays stateless, and the client still learns nothing about the host's machines, not even
> which of its own workloads share one. What it costs: a relay holds one more key, which reveals
> nothing a relay does not already see by carrying the session; a route outlives a migration by
> at most one token lifetime, and the node it names then refuses as `not_here`, so the client
> mints again; and the key is the token key's, so a restore that rotates one retires both.

**As built, as its own process.** A relay is enrolled, joining with its keys and receiving a
certificate under the installation's authority (§3 of `18-operations.md`), and then runs with
`korpis-server relay run`, serving the same two services from no database. It is fed what it
verifies by: `RelayControlService.Feed`, opened with a request its identity key signs over a fresh
nonce the control plane accepts once, sends the control plane's auth skew window first, then the
token keys with their route keys, the signed revocation list, and the keys of every node not
decommissioned, each when it changes, and a heartbeat on a look that sent nothing; it ends when the
relay is removed. The relay believes a list only once its signature verifies against the token keys
it was fed. It takes a feed silent for three heartbeat intervals for lost and opens another, and a
feed the control plane refuses to authenticate for a removal, and stops. It admits a node's
`DataHello` only for its own URL, by the key the feed sent, within the skew window, and with a
nonce it has not seen inside twice that window. Nonce memory is the relay's own, which is why the
URL is signed: a relay holding a node's hello cannot replay it to another relay, and the control
plane refuses a hello for any enrolled relay's URL, removed ones included. Every replica names to
the nodes it holds sessions with, in a `Relays` message sent when the session opens and within
about two seconds of a change, every relay not removed, and whether the control plane serves the
role too, which it does unless `--relay-role false` turns it off. What it does not do: with its
feed lost, a relay verifies by the last of each it received, which bounds a revocation it missed by
one token lifetime, the same bound §4.6 states for the agent; before the first terms arrive it
admits no node at all. A client finds where to open a session in `ServerInfo`, which lists the
relays as the replica last named them to nodes and says whether the control plane serves the role
too; any relay listed reaches every connected node, since each node attaches to all of them. A
relay renews its own certificate (§3 of `18-operations.md`), and `ListRelays` says whether each is
connected: the replica feeding a relay records it at most every thirty seconds, and a relay whose
feed was served within ninety is connected. Behind a load balancer, `--proxy-protocol-from` names
the balancer's addresses: a connection from one of them must open with a PROXY header (version 1 or
2) within ten seconds, and the client it names is the address `source_cidr` is judged by; a
connection from one of them without a header is closed, and a connection from anywhere else is its
own client, whatever it sends. The control plane takes the same flag, for the gateway's
`source_cidr` on API calls and for its own relay role. What the header cannot do: it is believed
because the balancer is, so a balancer that forwards a header its own client wrote hands that
client any address it likes; configure the balancer to write the header itself.

**What a relay can see, stated plainly.** TLS terminates at the relay, so it sees the plaintext of
the sessions it carries: console output, keystrokes, and file contents in transit. A relay is
operator infrastructure with the same trust as the control plane, and it is deployed on machines
the operator controls. End-to-end encryption between a browser and a node would require every node
to hold a publicly trusted certificate, which is the inbound-reachability requirement §3 exists to
avoid. The cost is recorded in §8 of `17-security.md` rather than implied away.

**SFTP, decided 2026-10-22.** A relay may also accept SSH, on a port of its own
(`--sftp-listen`), and serve the SFTP subsystem there as a client of its own file sessions: the
SSH user names the workload by its identifier, the client authenticates with an SSH key its person
registered (§9.9 of `08-identity.md`), and the relay, having checked that the client holds the
key, asks the control plane over its fed identity for a file token for that key's person, that
workload, and the client's address. The gateway evaluates the grants online as it does for
`MintToken`, and answers the token and the route, or a refusal; the relay then opens the session
as any client does, translates each SFTP request into the file session's requests (§3.2 of
`06-storage.md`), and asks again before the token expires. It is the relay, the tier that already
joins clients to nodes, so a 40 GB upload never crosses the control plane, and it is the file
session, so SFTP adds a protocol and decides nothing about files twice (§6). Off by default:
an operator turns it on per relay, and on one machine for the control plane's own relay role.

What it costs, stated plainly. No SSH server library hands its caller the client's signature: the
one the relay uses verifies it and passes on the key alone, as OpenSSH's `AuthorizedKeysCommand`
does, so the control plane cannot check that a client signed, and believes the relay that says
one did. A relay serving SFTP can therefore open a file session as anyone who registered a key,
on any workload that person's grants reach, a power no relay held before; it is bounded to file
actions, every token it asks for is recorded with its name, and a relay not serving SFTP gains
nothing. An operator who will not extend a relay that trust leaves SFTP off it. Each relay has
its own host key (`--sftp-host-key`, made at its first start where the file is absent), whose
fingerprint `ServerInfo` lists beside the relay's SFTP address, so a client checks the key it is
shown; relays behind one name are given one key file, or a client's next connection may reach a
relay whose key it has never seen.

**From a browser, decided 2026-10-16.** A session is one stream both ways (`RelayService.Open`),
and a browser cannot open one: the Connect protocol over `fetch` carries unary calls and server
streams, and no browser lets a request keep sending while its response is being read, Chrome's
half-duplex streams included. So the relay serves the same session two further ways, for any
client that cannot stream both ways. `RelayService.Watch` takes the opening message and streams
the session's frames back; `RelayService.Send` delivers the client's frames, each call naming the
session and numbering its frames, and the relay hands them to the node in order and once: a
retried call whose frames were already delivered is acknowledged and not delivered again, so a
dropped connection neither loses nor repeats a keystroke or a chunk of an upload. A `Send` that
reaches a process not holding the session, a replica behind a load balancer with no affinity, is
refused as `session.not_here`, and the client opens the session again; console output is a durable
stream (§4 of `14-streams.md`) and an upload resumes from the size already written (§3.2 of
`06-storage.md`), so the reopening loses nothing, and a balancer with session affinity avoids it.
What it costs: one call per batch of input instead of one stream, which a keystroke's latency
does not notice and an upload amortizes over chunks of a mebibyte; and a third shape of session
for the relay to keep, though the frames and the node's side are the same as `Open`'s.

> As built, 2026-10-17 (`crates/korpis-server/src/api/relay_watch.rs`). `Watch` opens the session
> exactly as `Open`'s first message does, and its first answer, `opened`, carries a `send_key` of
> 32 random bytes that only the watcher is told. `Send` names the session and that key, and the
> number of its first frame among all the client has sent; frames below what was delivered are
> skipped, a gap is refused, and the answer says how many are delivered, which is where the next
> call starts. A key that does not match is answered exactly as a session this process does not
> hold, so the session identifier, which the node and the logs see, is no handle on it. Each `Send`
> judges the session's current token again, from the address that call came from, so revocation
> stops what is sent and a grant limited to an address range holds for every frame, as it does for
> an `Open` stream, whose one connection has one address (Finding 78 of `23-walkthroughs.md`). A
> renewal sent through `Send` is judged as one sent through `Open` is, and becomes the token judged
> after it. The session lives as long as its `Watch` stream. A relay that is its own process
> answers cross-origin calls for the panel's origins it is given (`korpis-server relay run
> --web-origin`) and for the portals' its feed names (§3.6 of `07-networking.md`), and the control
> plane writes the relays it names into the policy of the panel page it serves, so the page may
> connect to them (§7 of `11-surface-web.md`).

**A direct path is an optimization, never a dependency.** A node that is reachable and holds a
certificate the client trusts may advertise a direct endpoint in `NodeStatus`. The CLI and SDKs,
which pin the Korpis CA, use it when present; browsers use it only when the node's certificate is
publicly trusted (§3.2 of `07-networking.md`). Every session works through a relay, so a node that
stops being reachable degrades in latency, not in function.

---

## 4. The protocol

### 4.1 Level-triggered, never edge-triggered

This is the single most important decision in the document.

The control plane does **not** send "start workload X". It sends **the complete set of assignments
for this node** (§4.8): for every workload placed there, the configuration and power state its
approved plans have reached or are reaching, the lease, and any plan steps, retirement, or
migration role in progress. The agent's
job is to make reality match that set, creating what is missing, retiring what it is told to
retire, and correcting what has drifted.

Edge-triggered systems ("start X", "stop Y", "delete Z") are correct only if no message is ever
lost, reordered, or duplicated. Every real network violates all three. When an edge-triggered
message is dropped, the state divergence is permanent: nothing will ever resend "start X", because
from the sender's perspective it already happened.

Level-triggered systems self-heal. A dropped message is repaired by the next sync, because every
sync carries the whole truth rather than a delta from an assumed prior state.

The concrete consequence for a workload: **there is no `restart` command in the protocol.** Restart
is a new `Power` version whose `cycle` counter is one higher than the last (§3.3 of
`01-model.md`). The agent sees a power cycle it has not yet performed and performs it. If the
message is lost, the next sync carries the same `Power` and the same convergence happens. The
agent records the last cycle it performed beside the runtime object, so a cycle is performed once
however many times it is delivered. Nothing is missed and nothing is done twice.

> Second external review, finding S2. An earlier version of this paragraph described restart as
> `Intent.state: running` plus a `restart_epoch` field. Neither field existed after finding R7 moved
> power out of the intent body, and nothing carried `Power` to the node at all. §4.8 is the carrier.

Deltas are permitted as a bandwidth optimization, but only as a compression of a level-triggered
stream: every delta names the set digest it applies to, and a mismatch triggers a full resync. The
agent is never required to have received any particular earlier message.

### 4.2 The stream

One bidirectional stream, multiplexed. It carries decisions and facts, never bulk data: console
bytes, file transfers, and stream segments travel on the separate data connection of §3.1, so a
40 GB upload can never delay a lease renewal.

**Control plane → agent**

| Message | Purpose |
|---|---|
| `AssignmentSet` | the complete set of assignments for this node (§4.8), each signed, with the set digest |
| `AssignmentDelta` | optimization only; carries the base set digest it applies to |
| `LeaseGrant` | the answer to a `LeaseRequest`, echoing its nonce: an epoch and a term (§4.5) |
| `LeaseRevoke` | an optimization that hastens a release; safety never depends on its delivery (§4.5) |
| `RevocationList` | revoked and suspended grant identifiers, signed by the token key, for offline verification (§6.3 of `08-identity.md`) |
| `JournalAck` | the highest journal sequence durably ingested, so the agent may discard up to it (§4.10) |
| `FeasibilityQuery` | a side-effect-free `Prepare` against a supplied manifest, for placement (§2.1 of `05-scheduling.md`) |
| `ContentHint` | recipe digests likely to be needed soon; prefetch |
| `Probe` | liveness check with a nonce |
| `ReachabilityProbe` | listen on a port of the node's range, over TCP and UDP, and answer nonces with the node key's signature, to measure whether the node can be reached at what it advertises (§9 of `07-networking.md`) |
| `Welcome` | the first answer to a verified `Hello`: the protocol accepted, the heartbeat interval, the auth skew window, the control plane's clock, and the metering interval |
| `Relays` | the relays to keep a data connection to, and whether the control plane serves the role itself; sent when the session opens and when they change (§3.1) |
| `WorkloadCredential` | the answer to a `CredentialRequest`: a credential for the workload and its expiry, or the refusal (§9.5 of `08-identity.md`) |
| `JobAccess` | the answer to a `JobAccessRequest`: the workload's data key and scoped access to its repository's store, or the refusal (§5.5 of `06-storage.md`) |
| `StepResolution` | the answer to a `StepRequest`: what the provider step resolved to, or the refusal (§4 of `09-recipes.md`) |
| `CertificateKey` | the answer to a `CertificateRequest`: the certificate's chain and key at its revision, or the refusal (§3.2 of `07-networking.md`) |

**Agent → control plane**

| Message | Purpose |
|---|---|
| `Hello` | agent version, protocol version, driver capabilities, storage classes as the helper declares them (§2 of `06-storage.md`), node capacity, what it advertises for inbound traffic (§9 of `07-networking.md`), the set digest it holds, whether it answers a `FeasibilityQuery` |
| `Observation` | current observed state of one workload, including what it has converged to (§4.3) |
| `NodeStatus` | the heartbeat: capacity, allocation, pressure signals, driver health, what it advertises for inbound traffic (§9 of `07-networking.md`), and a digest of current observations |
| `JournalRecords` | effects, metering samples, and break-glass records, in sequence order (§4.10) |
| `LeaseRequest` | acquire or renew a lease, carrying a nonce; the clock starts when it is sent (§4.5) |
| `LeaseReleased` | the node has stopped acting under an epoch; it is also a journal record |
| `FeasibilityResult` | the answer to a `FeasibilityQuery`: accepted, rejected with a reason, or unanswered when the driver could not be asked |
| `ContentStatus` | which recipe digests are present locally, all of them, on every new session and whenever one is added; each replaces the last (§2.2 of `05-scheduling.md`) |
| `Ack` | the assignment set digest the agent has received |
| `CredentialRequest` | a workload asked its identity socket for a credential; names the workload and a request identifier (§9.5 of `08-identity.md`) |
| `ProbeListening` | the node is listening for a `ReachabilityProbe`, or why it could not |
| `JobAccessRequest` | a backup, restore, or offload asks for what it needs; answered only to the node holding the workload's lease whose assignment carries the step (§5.5 of `06-storage.md`) |
| `StepRequest` | an install asks for one of its provider steps to be resolved; answered only to the node the workload is placed on (§4 of `09-recipes.md`) |
| `CertificateRequest` | the proxy needs a certificate an assignment names; answered only to a node an endpoint served with it is placed on (§3.2 of `07-networking.md`) |

> Corrected 2026-10-16, when the node's certificate fetch was built. These tables lacked the
> job-access and step-resolution exchanges, which were added to the protocol when backups (P1.3)
> and provider steps (P1.4) were built and never listed here; a table of the protocol that is
> missing messages is a restatement the corrections did not reach. They are listed now, beside the
> certificate fetch.

> Corrected 2026-10-14, from the implementation. `NodeStatus` was said to carry the node's
> reachability. A node cannot know that from the inside (§9 of `07-networking.md`); it carries
> what it advertises, and the control plane measures reachability against it with
> `ReachabilityProbe`.

> Added 2026-10-13, from the implementation: `Welcome`. The heartbeat interval was declared
> (question 6 of §10) and nothing carried the declaration to the agent, so an agent could only
> guess how often to send what three missed would condemn it for.

> Corrected 2026-10-14, from the implementation: the relays moved from `Welcome` to `Relays`. A
> `Welcome` is sent once per session, and relays are enrolled and removed while sessions stay
> open, so a node told only at `Welcome` would keep attaching to a removed relay until it
> reconnected.

> Second external review, findings S1 and S19. Recorded in §17 of `23-walkthroughs.md`. The first
> version of this table carried `AssignedIntents`, which could not express a power change, an
> approved-but-unapplied plan, a deletion, or a migration role, and it carried console traffic on
> the control stream while §4 of `14-streams.md` sent clients directly to nodes that, under §3, have
> no inbound port. Both are corrected by §3.1 and §4.8.

### 4.3 Convergence is measurable

Every assignment carries a monotonic `generation` (§4.8). Every `Observation` reports what the agent
has converged to, field by field: the `assignment_seen` generation, the `intent_seen` (the target
intent's identifier), the `power_seen` version and cycle, and `plan_progress`, the last plan step
it completed.

Those fields make the whole system honest:

- every reported field matches the current assignment, no plan steps are outstanding, and the
  observed state agrees with `Power.desired` → **converged**
- `assignment_seen` trails the generation at which what the assignment has the node run last
  changed → the agent has not received or not yet applied it. The UI says "applying", with the
  exact lag.
- `assignment_seen` matches, plan steps are outstanding, and the last one reported progress within
  its declared timeout → **applying**, and the UI names the step.
- `assignment_seen` matches and the observed state still disagrees with the assignment, with no
  step outstanding → the agent has the assignment and cannot satisfy it. This is a real failure
  and is surfaced as one, not as a spinner.
- No observation within the staleness window → `unknown`. Never a remembered value presented as
  current (P4).

An observation is **current** only while it comes from the node the workload is placed on and that
node is heard; the staleness window is the node's, three missed heartbeats (question 6 of §10), not
a per-workload clock. One that is not current is still shown, with its time, and with its state
`unknown`, so a reader sees what was last known without mistaking it for now. A node's observation
is believed only for a workload it holds an assignment or placement for, as its journal records are
(§4.10); the latest replaces the one before, and history is the effect log. `intent_seen` is
compared with the approved target, `power_seen` with the approved power declaration's version and
cycle, `assignment_seen` with the generation at which what the node's assignment has it run last
changed, and the state with `Power.desired`. A state the node reports as crashed or unsatisfiable
is failing whatever else matches; a transitional state (starting, stopping) with everything else
matching is applying.

**A generation that changes nothing the node runs does not unsettle a converged workload.** Decided
2026-10-16. The generation rises with every change of the assignment (§4.8), including two that
change nothing the workload runs: dropping a plan once it is applied, and carrying an ACME
challenge while a certificate is ordered (§3.2 of `07-networking.md`). Compared with the generation
itself, a workload that had converged read `applying` again until its node took the new one, a
flicker the backup tests caught and had to wait out. So the control plane keeps, beside each
assignment, the generation at which it last changed in anything but its plan and its challenges,
and compares the node's report with that; a plan is measured by its own progress, which already
reads `applying` while it is. What it costs: one more digest per assignment computed, and a node
that has not yet taken a generation which only drops a finished plan still holds that plan, which
it has completed and does nothing with.

Two states that are not about the node at all are computed in the control plane, never inferred
from an observation: a declared intent waiting for approval (`Workload.intent` ahead of
`Workload.target`, or the power head ahead of `Workload.power_target`, with a plan proposed for the
newer one, §3.3 of `01-model.md`) shows as **awaiting approval**, and an approved plan with a
future window shows as **scheduled**. Neither has reached a node, so no node can report it. The
third is **unplaced**: an approved target with no authoritative placement bound for it, because
no node could take it (§2.3 of `05-scheduling.md`). It is shown with the explanation of why every
candidate was rejected, filtered for the viewer (§8 of `08-identity.md`), and it is never a
spinner: the control plane retries it, and the state changes only when a node can take it.

> Second external review, findings S2 and S3. The first version compared `intent_seen` with
> `Workload.intent` and `Observation.state` with `Intent.state`. After finding R7 there was no
> `Intent.state`, and `Workload.intent` moved at declaration, before approval, so the comparison
> was either undefined or measured convergence toward something nobody had approved.

### 4.4 Adoption: a restarting agent must not kill anything

When the agent starts (after a crash, an upgrade, or a reboot), running workloads must survive.

Every runtime object the agent creates is labelled at creation with `workload_id`, `intent_id`,
`assignment_generation`, `power_cycle`, and `lease_epoch`. The intent is labelled by its
identifier, not its version number: identifiers are never reused, while a version number can be
issued twice across a control-plane restore, and a label that can mean two things cannot be
trusted to say which one was applied (§8 of `03-state.md`). On startup the agent:

1. Enumerates all runtime objects across all drivers.
2. Matches them to assignments by `workload_id`.
3. **Adopts** matches: takes over supervision without touching the process.
4. Reconciles differences: if the adopted object was created from an older target, apply the
   difference; if it can only be corrected by recreation, that is a step with a `disruptive` flag.
5. Reports objects with no matching assignment as orphans. It does **not** delete them
   automatically, because "the control plane forgot about it" and "this should be deleted" are
   indistinguishable from the node's point of view, and one of them destroys customer data.
   Deletion is the separate, explicit `retire` of §4.8, which a forgotten object never carries.

An agent upgrade is therefore a process restart, not a workload restart.

**What the implementation records** (2026-10-13). Each adoption is a journal effect,
`node.adopt`, naming the workload and the runtime object, so an agent restart that kept a
workload running is visible in the effect log rather than inferred from the absence of a
restart. Each orphan is a `node.orphan` effect naming the runtime object and the workload its
label claims, reported once per agent start. A second object labelled with a workload that
already has one is an orphan too: the agent keeps supervising the first and touches neither.

**Adoption comes first, however long the driver takes to answer.** Corrected 2026-10-19 (Finding
109 of `23-walkthroughs.md`). An agent whose driver cannot enumerate its objects at start, a
containerd too loaded to answer within its deadline, reconciles nothing until the driver can: it
asks again after half a second, then twice as long each time up to fifteen seconds, and acts on no
assignment meanwhile. The first build logged the failure and went on reconciling with nothing
adopted, so each workload already running was prepared and created a second time beside itself.
The cost is that a node whose driver stays unable to answer runs what it ran, unsupervised and
unreported, until it answers; its lease expiring is what tells the control plane (§4.5).
Effects a node originates are named in two namespaces, `step.<action>` for a plan step's
completion and `node.<event>` for what the node did on its own (`node.adopt`, `node.restart`,
`node.superseded`, `node.orphan`), so that none can be mistaken for the control plane's own
effects of the same subject, such as `workload.retire`.

### 4.5 Leases and fencing

**Every running workload requires a valid lease. No lease, no execution.**

```
Lease
  workload      WorkloadID
  node          NodeID
  epoch         int          monotonic, per workload, never reused, signed by the control plane
  term          Duration     how long the lease lasts, measured by the holder (below)
  on_expiry     keep_running | stop       resolved from the intent, never defaulted here
```

The control plane issues exactly one valid lease per workload at any time. The epoch increments on
every issue.

This is what Pterodactyl lacks, and it is why its transfer failures deadlock. When a transfer is
interrupted, both nodes may believe they own the server, the panel cannot express that state, and
the recovery procedure is truncating a table by hand (issue #4505). With leases, the answer is
mechanical: whoever holds the current epoch owns it.

#### Exclusivity is a property of the clocks, not of message delivery

> Second external review, finding S7. Recorded in §17 of `23-walkthroughs.md`.

The first version of this section made exclusivity depend on `LeaseRevoke` arriving before
`LeaseGrant`, which is an edge-triggered message, the kind §4.1 says every real network loses, and
it expressed expiry as an absolute timestamp that the node compared against its own clock, which
§4 of `03-state.md` already treats as untrustworthy. Neither holds. Exclusivity now rests on a
rule that needs no synchronized clock and no reliable delivery:

1. **A lease is acquired or renewed by request.** The agent sends `LeaseRequest` with a nonce and
   notes the time **on its own monotonic clock at the moment it sends**. The grant echoes the nonce.
2. **The holder's lease runs from the send, the grantor's from the receipt.** The holder treats the
   lease as valid until `sent_at + term × (1 − ρ)`, where ρ is the declared bound on clock-rate
   error. The control plane treats it as valid until `received_at + term`. Real time only moves
   forward between sending and receiving, so the holder always believes its lease ends **before**
   the grantor does, whatever the network delay. This is the classical lease argument and it needs
   no shared clock.
3. **A new epoch for a different node is issued only when the old lease is provably over:** either
   the holder has sent `LeaseReleased` for that epoch, which it does only after it has stopped
   acting under it, or the grantor's own end of the old lease has passed. `LeaseRevoke` asks the
   holder to release early; when it is lost, the handoff waits one term instead of becoming unsafe.

So there is no window in which two holders are both authorized, and the claim no longer depends on
a message nobody can guarantee. The cost is a handoff that can take one lease term when the source
is unreachable, which is why terms are short and stated.

**The stated values.** A term is 60 seconds, configurable per installation, and ρ is 1,000 parts per
million. A quartz clock drifts by tens of parts per million and a disciplined one by far less, so
ρ is a margin against a clock that is worse than it should be, at a cost of 60 milliseconds per
term. Sixty seconds is the longest a handoff from an unreachable node waits, and the longest a
`stop` workload keeps running after its node loses the control plane; shorter terms buy a faster
handoff with more renewals, and the agent renews well before the end (a third of the term), so a
renewal lost to one dropped packet costs nothing.

**How the holder keeps its side**, from the implementation (2026-10-13). A request unanswered
within five seconds, or a third of the term if that is shorter, is sent again with a fresh nonce,
and an answer whose nonce is not the one awaited is ignored, so a grant delayed past its retry
cannot extend a lease from the wrong send time. A refusal for `UNAVAILABLE` or `NOT_ASSIGNED`
does not shorten a lease already held: its validity is a property of the clocks above, and it
runs to the end the holder counted, with nothing mutated after that. A `SUPERSEDED` refusal ends
it at once, since the control plane issues another node an epoch only once this one's lease is
provably over. An assignment whose `on_lease_expiry` arrives unspecified, which a resolved one
never is, is treated as `stop`, the answer that cannot make two writers. A lease the node has
released is not asked for again while the workload's assignment is still held, which it is
between a retirement finishing on the node and its completion being ingested; asking would
undo the release.

**What a request is answered with**, given where the workload is placed and who holds its lease:

| The node asking | holds the lease, unreleased | another node holds an unexpired lease | nobody does |
|---|---|---|---|
| is where the workload is placed | renewed, **same epoch** | refused, `UNAVAILABLE`: retry once the old lease is over | a new epoch |
| is not | refused, `NOT_ASSIGNED` | refused, `SUPERSEDED`, naming the holder | refused, `NOT_ASSIGNED` |

A renewal is not an issue, so it keeps the epoch: the epoch labels runtime objects (§4.4) and
travels in the assignment, and an epoch that moved on every renewal would make every renewal a new
assignment. The epoch increments on every *issue*, to a node acquiring the workload or to the same
node acting again after it released it; epochs come from one sequence for the installation, so
they are never reused, and a restored control plane raises the sequence's floor (§8 of
`03-state.md`). The node's own unreleased lease is renewed whatever epoch the request says it
holds, which is the fail-static row below: an agent that restarted, or a control plane that was
restored, gets back the epoch that is still its own. Issuing an epoch is an effect,
`lease.issue`, naming the holder it replaced; a renewal is not, since it changes nothing anyone is
told. `LeaseRevoke` is not sent in Phase 0, where a workload leaves a node only by being retired;
it arrives with migration, and safety never depended on it.

**The epoch check is in the data path wherever the storage allows it.** Leases fence the agents.
They do not, by themselves, fence a disk that a second node could attach. A storage class reachable
from more than one node declares `fencing: storage_enforced` when the backend can refuse a
superseded writer (SCSI-3 persistent reservations, a Ceph exclusive lock with blocklisting, an NFSv4
lease), and `fencing: lease_only` when it cannot (§2 of `06-storage.md`). A `lease_only` class is
honest about relying on the agents alone, and the default of the next section is chosen
accordingly.

#### What a fenced agent does depends on why it was fenced

> Second external review, finding S8.

The first version said in one paragraph that a fenced agent's workloads keep running, and in the
next that "everyone else stops", and §9 said a returning node stops its workloads. All three were
describing different situations without saying so. There are exactly two:

| Situation | How the agent learns it | What it does |
|---|---|---|
| **Its epoch is stale, and nobody else holds a newer one** (a restored control plane, a partition that healed before anything was reassigned) | its `LeaseRequest` is answered with a reissue for the same node | freeze mutations, keep processes running, accept the reissued epoch, resume. Fail-static (R12, §8 of `03-state.md`) |
| **Its epoch was superseded by a grant to another node** | the workload is absent from its assignment set and the refusal names the superseding node | stop the workload, retain its data, report the volume as an orphan for an operator decision. Another copy is authorized, and two running copies of a game server are two diverging worlds |

The control plane always knows which case it is in, because it issued the newer epoch or did not,
and it says so in the refusal. The agent never has to guess.

#### `on_expiry` is declared per workload, and its default is derived, not global

> Second external review, finding S8.

`on_expiry` decides what a workload does when its lease ends and cannot be renewed. The correct
answer differs by workload:

- A game server with local storage: `keep_running`. If the control plane is unreachable, players
  are still connected and stopping them serves nobody, and nothing else can use that disk.
- A stateful workload on storage another node could attach: `stop`. Two writers is data
  corruption. Losing availability is recoverable; losing the database is not.

So the field lives on the intent (`continuity.on_lease_expiry`, §3.3 of `01-model.md`), and when it
is not set the default is **derived** from the volumes the workload attaches: `stop` if any of
them is in a class that more than one node can attach, `keep_running` otherwise. The first version
said here that a global default would be wrong, while §8 of `03-state.md` and §5 of
`18-operations.md` named `keep_running` as the default; the derived rule is what all three now say.

**`keep_running` has a consequence for rescheduling, and it is enforced rather than hoped for.** A
workload that keeps running when its node is unreachable may still be running, so placing a second
copy elsewhere is not recovery; it is a split. The scheduler therefore never reschedules a
`keep_running` workload automatically when its node becomes unreachable. An operator may, through an
`evacuate` Operation (§3 of `05-scheduling.md`) that names the risk in its plan, and when the
original node returns it is in the second row of the table above.

### 4.6 What happens when the control plane is down

This is the payoff of P1, and it should be stated as a guarantee rather than a hope.

| Capability | Control plane down |
|---|---|
| Running workloads | **keep running** |
| Crash restarts, health checks, drift correction | **keep working**, the agent has the intent and does not need permission to execute a decision already made; a crash restart under a lease that expired is the one change a `keep_running` workload still gets, a restart for a failing liveness probe counted as one (§5 of `04-runtimes.md`), and it waits a second after the first crash, doubling to five minutes, forgiven after a minute up |
| Leases with `on_expiry: keep_running` | honoured until the control plane returns |
| Leases with `on_expiry: stop` | workload stops when the lease expires, by design |
| Plan steps already assigned to the node | continue to the end of their assignment; nothing new is assigned |
| Effects, metering, break-glass use | recorded in the node's journal and delivered on reconnect (§4.10) |
| New intents, plans, placements | unavailable |
| Established console and file sessions | continue through the relay for the remainder of their token's lifetime (§3.1) |
| New console and file sessions | unavailable: issuing a token requires the gateway |
| Web, CLI, chat, API | unavailable |

The data plane does not depend on the control plane to keep doing its job. That is the difference
between a panel and a platform.

New console sessions during a control plane outage are a real gap and are treated as one: the
break-glass socket of §10 is the answer for the operator, and it is deliberately not an answer for
anyone without a shell on the machine.

### 4.7 Idempotency

Every plan step is keyed by `(plan_id, step_index)`. Applying a step that has already been applied
is a no-op that reports success. Retry is therefore always safe, which is what allows every
operation to be retried rather than requiring a distinction between "failed" and "possibly failed"
(P9).

> Second external review, finding S4. Recorded in §17 of `23-walkthroughs.md`. The key was
> `(workload_id, intent_version, step_index)`, which is unique only for plans that create an
> intent. A power change, a drain, or a second migration of an unchanged workload produces the
> same key as an earlier plan, and "a repeated key is a no-op that reports success" then makes the
> second migration succeed without moving anything. A plan identifier is unique by construction.

Drift correction is not a plan and has no step key. It is idempotent for a different reason: it
converges toward the assignment, and converging toward something already reached does nothing.
The effects it emits are deduplicated by the journal sequence of §4.10, not by a step key.

### 4.8 The assignment: what a node is told

> Second external review, findings S1, S2, S3, and S5. Recorded in §17 of `23-walkthroughs.md`.

The first version of this protocol sent a node "the complete set of intents placed on it". That set
could say what configuration a workload should have, and nothing else. It could not say whether the
workload should be running (R7 had moved power out of the intent), which plan was being applied
(§7 of `03-state.md` needed to cancel one mid-flight), whether a workload had been deleted rather
than forgotten (§4.4 cannot tell the difference), or what a node's part in a migration was (the
seven phases of §8 of `05-scheduling.md` are ordered actions on two nodes). Each of those was being
described elsewhere as a message the protocol did not contain.

**The unit of desired state on a node is the assignment, one per workload placed there:**

```
Assignment
  workload      WorkloadID
  owner         {project, organization}
  name, labels  the workload's, which the node does not use: what a restore that lost the
                               workload recovers it under (§8 of 03-state.md; Finding 95)
  generation    int            monotonic per workload; any change below increments it
  target        Intent         the configuration in full that the workload's plans reached, or
                               that the plan in `plan` is reaching; never the head, and the
                               target only once a plan reaches it; the agent needs the body
  power         PowerRef       likewise for power: desired state, version, and cycle (§3.3 of
                               01-model.md); stopped at version zero while no plan has brought
                               the workload to any intent
  continuity    on_lease_expiry, resolved (§4.5)
  shaping       quota-imposed state, e.g. an exhausted egress budget (§3.1 of 01-model.md)
  plan          NodePlan?      present while an applying plan has steps this node executes
  retire        RetireRef?     present when the workload is to be removed from this node
  migration     MigrationRole? present during a migration: role, phase, peer
  ids           IdRange        the host ids the workload's user namespace maps onto, unique
                               on this node (§4.5 of 04-runtimes.md)
  lease_epoch   int            the epoch the node holds the workload's lease under (§4.5)
  recipe_manifest  bytes       the resolved recipe the target names, so the node needs no
                               registry to know what to run
  residual      bool           what a retired workload left on the node: its plan, and nothing
                               else (§7 of 06-storage.md)
  volume_ids    map            the helper's identifier of each existing volume the target
                               attaches (§7 of 06-storage.md)
  endpoints     []Endpoint     the target's endpoints as placed here, with the node ports they
                               hold, and for the ingress their names and certificates (§3 of
                               07-networking.md)
  dependencies  []Resolved     its dependencies, resolved to what they reach now (§5 of
                               07-networking.md)
  reaches_api   bool           whether the node relays the API to it (§9.5 of 08-identity.md)
  challenges    []AcmeChallenge  the HTTP-01 challenges its node's proxy answers while its
                               endpoints' certificates are ordered (§3.2 of 07-networking.md)
  runs          []RunRef       the active runs of a task or scheduled workload, which the node
                               executes; none for a persistent one (§4.5 of 05-scheduling.md)
  kept_runs     []RunID        every run of it the control plane still holds, in any status;
                               the node deletes a run's stream only once the run is not here
                               (§4 of 14-streams.md)
  signature     the control plane's signature over all of the above

NodePlan
  plan_id       PlanID
  steps         []NodeStep     the steps this node executes, in order, each with its index
  through       int            the last step index the node may execute
  whole         Plan           the plan in full, its status and progress left out, which the
                               node does not use: what a restore that lost it recovers it from
                               (§8 of 03-state.md)

RunRef
  run           RunID
  attempt       int
  trigger       schedule | manual | retry | event
  scheduled_for Timestamp      also what a restore that lost the run records it under
  timeout       Duration?      enforced by the node (§4.2 of 05-scheduling.md)
  intent        IntentID       the intent it was made under, whose exit probe judges it
  cycle         int?           the power cycle that requested it; exactly for a manual run

MigrationRole
  role          source | destination
  phase         prepare | replicate | quiesce | final_sync | verify | cutover | release
  peer          NodeID
```

> Corrected 2026-10-16, when the issuer of certificates added `challenges`. The block above
> listed the assignment as first designed; the fields added as volumes, residual plans,
> endpoints, dependencies, and the API relay were built were each documented where they were
> built and never added here. They are listed now; `migration` and `shaping` are still designed,
> not built.

> Added 2026-10-18, when runs were designed (Finding 98 of `23-walkthroughs.md`): `runs` and
> `RunRef`, designed and not yet built, are how a node learns which runs to start and which to
> stop, since the target intent says how to run the workload and not when. Built the same day
> (`crates/korpis-app/src/node/assign.rs`): signed with the rest, and counted in what the node
> runs, so a run made or ended sends the node a new generation. `intent` and `cycle` were added
> the same day, when a restore was built to recover runs (§8 of `03-state.md`): the run table
> cannot hold a manual run without its cycle, and a run recovered under any intent but its own
> would be judged by another exit probe. `kept_runs` was added the same day, when retention was
> built to take a run's stream with it: the node holds each run's stream, and nothing else tells
> it when retention has deleted the run.

The assignment is still level-triggered. It describes what should be true, and the node makes it
true; nothing in it is an instruction that is lost if a message is lost. Each part answers one of
the gaps above:

- **`target` is the approved intent, not the newest one.** A declaration that is waiting for
  approval has not reached any node, because it is not in any assignment. The agent therefore
  cannot converge toward a change nobody approved, which is the property P5 depends on and which
  the first version did not have (§3.2 of `03-state.md`).
- **`power` carries start, stop, and restart, once approved.** A restart is a higher `cycle`; the
  agent performs each cycle once and records that it did (§4.1). Like `target`, it is an approved
  declaration, so a stop waiting for confirmation has not reached the node (§3.3 of
  `01-model.md`, corrected 2026-10-13).
- **Approved is not enough: `target` and `power` are what a plan made or is making.** Approval
  moves the workload's target before anything runs, and the agent levels toward what it is told,
  so a node told the target runs what no plan has made. It is told instead the intent and power
  the workload's plans reached, or that the plan in `plan` is reaching: the target once a plan
  reaches it, and until then the last one reached. A plan approved for a later window
  (`apply_after`) changes nothing on the node before it is due, nor one queued behind another plan
  before it begins. A workload no plan has brought to
  any intent is told its target, which the plan's steps need, with power stopped at version zero,
  so the node keeps the assignment, and what the steps saved, and runs nothing.

  > Corrected 2026-10-15, Finding 52 of `23-walkthroughs.md`. `target` and `power` were the
  > workload's approved ones. A create approved for a later window started at once, without its
  > pull, volume, or install; a create whose plan failed started without its install; and a change
  > waiting for its window was applied by the next crash restart, which rebuilds from the target.
- **A residual assignment carries a plan for what a workload left behind.** A retained volume
  outlives its workload (§3.6 of `01-model.md`), so deleting one, or taking, rolling back to, or
  deleting one of its snapshots, is a plan for a workload that may no longer run anywhere. Its
  node is sent an assignment for that workload marked `residual`, carrying the plan on the volume
  and, for the record, the last target and power the
  workload had. The agent executes the plan's steps and nothing else: it builds, starts,
  observes, and leases nothing for it. No lease fences those steps, because what they touch is
  on that node alone, which is why a plan touching anything reachable from a second node can
  never be carried this way. The assignment leaves the set when the plan is done (§7 of
  `06-storage.md`). Added 2026-10-14, when deleting retained volumes was built.
- **`plan` carries ordered work without becoming a command.** It states which steps of which plan
  this node is to have executed, up to `through`. The agent executes the next unexecuted step,
  reports its completion as an effect keyed `(plan_id, step_index)`, and moves on. **Cancelling a
  plan is lowering `through`** below every step not yet started: the agent finishes the step in
  flight and does not begin the next. A lost update is repaired by the next sync, like everything
  else, and `plan.cancel` (§7 of `03-state.md`) is a change to desired state rather than a message
  that has to arrive.

  > Corrected 2026-10-13, from the implementation. "The last step already started" is a fact
  > only the node holds, and an index cannot say "none" when no step has completed, so a
  > cancelled plan simply leaves the assignment, which is the lowest `through` there is. The agent
  > reads its assignment again before every step, not only when a pass begins: one that read it
  > once would run every remaining step of a pass before seeing the cancellation.

  > Added 2026-10-13, from the implementation: how a node executes steps. A completion is
  > journalled before the agent keeps, in its own state, that the step is done, so a crash
  > between the two runs the step again; the step is a no-op the second time and its second
  > completion is recorded and moves nothing. A failed step is journalled and the agent does not
  > go past it: the control plane fails the plan, and the next assignment no longer carries it.
  > `reconfigure` validates the new target with the driver's `Prepare` and changes nothing
  > running; the runtime object is rebuilt from the approved target whenever it is next started,
  > so a reconfiguration takes effect at the next start or restart, never later and never
  > unannounced. A node-local volume is detached when its runtime object is rebuilt or
  > destroyed, which is what `detach_volume` records.
- **`retire` is how deletion differs from forgetting.** A workload whose assignment carries
  `retire` is stopped and its runtime objects destroyed; its volumes are not touched, because
  deleting a volume is its own irreversible plan step (§3.6 of `01-model.md`), which follows
  `retire` in the same plan only for a volume the deletion named (§7 of `06-storage.md`). Once
  the agent reports the retirement complete and the effect is ingested, the assignment leaves the
  set, the placement is removed, and the workload's identity is retired (§3.3 of `01-model.md`). A
  runtime object with no assignment and no retirement is still an orphan (§4.4) and is still never
  deleted on the node's own initiative.
- **`migration` makes each phase a desired state on each side.** The control plane advances the
  phase on both assignments after it has observed the previous one complete, and every phase is
  idempotent (§8 of `05-scheduling.md`). The ordering lives in the control plane; the node only
  ever converges to "you are the destination, in `replicate`, from node 4".
- **`signature`** lets an agent prove what it was told. §8 of `03-state.md` uses that to recover
  assignments a restored control plane no longer remembers, and it means a compromised relay or
  proxy on the path cannot alter an assignment unnoticed. The agent believes a set only when every
  assignment in it verifies under a key enrollment gave it, names this node, and the set's digest
  is the digest of what it carries; a set that fails any of these is refused whole, reported as a
  security event, and not acknowledged, and the node keeps the set it held. A believed set is kept
  on disk as it arrived, signatures included, and verified again when the agent starts, so what is
  on disk is believed no more than what arrives.

The set digest of §9 of `10-api.md` is computed over the assignments, generation by generation, so
the resync optimization is unchanged.

**How a generation moves.** The control plane recomputes a node's set whenever something it is made
of changes (a target or power target moving, a plan's status, a placement) and on every resync. An
assignment whose content is unchanged, judged by a digest over it with its generation zeroed, keeps
its generation and its signature and is not sealed again; one whose content changed is sealed one
generation higher. An assignment that leaves a node's set is kept, inactive, rather than deleted,
so a workload placed on the same node again continues from the generation it had: a generation that
started again at one would look, to an agent still holding a higher one, like going backward. The
computation of one node's set is serialized, so two control-plane processes cannot seal two
different assignments under one generation. The digest of the set is over each workload and its
generation, sorted by workload, as `sha256:` and hexadecimal; the agent computes the same over what
it holds.

**An approved plan becomes `applying` when it is first assigned** (§7 of `03-state.md`): the plan
the node is told to execute is the applying one, or, if none is, the earliest approved, which is
marked applying in the transaction that seals the assignment carrying it. A plan approved while
another is applying is queued, and §7 of `03-state.md` says when it preempts the applying one by
lowering its `through`. That lowering waits on the ingestion of step completions (§4.10), which is
what tells the control plane the last step started; until it lands, a queued plan reaches the node
when the applying one is done, while the approved power and target the queued plan moved are in the
assignment at once.

**What Phase 0 resolves without the information to do better.** `on_lease_expiry` derived from the
volumes (§4.5) needs to know whether a volume's class can be attached from a second node, and no
storage class a Phase 0 node offers can: every one is node-local, so the derived answer is
`keep_running`. A class that declares multi-node attachment, with the fencing of §2 of
`06-storage.md`, changes the answer, and the derivation already takes it. Every change sends the
whole set; the delta of §9 of `10-api.md` is an optimization not yet made, and its absence costs
bandwidth, never correctness.

### 4.9 Which steps run where

A plan's steps do not all run on the node. Allocating an address, holding quota, and asking a DNS
provider for a record happen in the control plane or through an extension. Each step declares its
executor:

| Executor | Runs | Carried by |
|---|---|---|
| `control_plane` | in a control-plane worker, claimed from the job queue (§6 of `03-state.md`) | the job queue, keyed `(plan_id, step_index)` |
| `node` | on the agent of the node in the assignment | `Assignment.plan` |
| `extension` | in the provider extension that owns it (§4 of `16-extensions.md`) | the extension contract, same key |

The plan's order is global across executors. The control plane does not place step *n* in an
assignment until steps before it have completed, whatever executed them, and it does not mark the
plan `applied` until every step's completion effect has been ingested. That is how a plan with steps
on two nodes and one extension is still one ordered, resumable sequence.

**Control-plane steps, as built, 2026-10-14**, when prunes made `prune_backups` the first of
them. A step whose turn has come (its plan approved or applying, its schedule passed, every step
before it complete) is queued as a job keyed by its plan and index, by whichever replica notices
first, on every change to a plan and on the change feed's periodic resync; any replica's worker
claims it, does it, and records its completion as a node's is recorded, in the transaction that
ends the job. An attempt that fails is tried again, half a minute later and doubling to half an
hour, and the eighth failure fails the step and so the plan. Corrected the same day (Finding 40
of `23-walkthroughs.md`): an assignment carried a plan's node steps through the last of them,
whatever came between, so a node would have run past a step the control plane had not done. Its
`through` now stops before the first control-plane step not yet complete, and moves on once it is.

**A node step may outlast a pass of the agent's loop.** Added 2026-10-14, when backups were built.
A backup step runs for as long as a volume takes to read, and the loop that executes steps serves
every workload on the node, so such a step does not hold it: each pass asks the helper where the
step's job is, starts it only when the helper knows none by the step's name, moves on while it
runs, and records the step's completion once the job has ended and its result is in the journal.
Asking the control plane for the job's key and access does not hold the loop either: the step asks
and moves on, and a later pass finds the answer, which can take as long as a backup target takes to
grant. A job is named once for its plan (`bkp-<backup>`, `rst-<job>`), so an agent that restarts
finds the one already running rather than starting a second.

A step does not forget its job. Done again after a crash, it must find the job it began; and a
replacing restore's staging place outlives the step that made it, until a later step of the same
plan commits it (§5.4 of `06-storage.md`). Corrected 2026-10-14, when restores were built: the
backup step forgot its job once it had journaled the result, so an agent that crashed between
the two would start the backup again. The agent now forgets a job once no plan in its set names
it, never one still running and nothing before a set has been held, which also releases a
snapshot or a staging place left by a plan cancelled midway.

### 4.10 What the node records: the journal

> Second external review, finding S6. Recorded in §17 of `23-walkthroughs.md`.

Most effects are things that already happened on a node: a restart, a console keystroke, a
break-glass session, a drift correction during an outage. §1 of `03-state.md` promised that an
effect commits in the same transaction as the state change it describes, which is true of
decisions made in the control plane and cannot be true of events on a machine that was not even
connected at the time. Nothing made their delivery reliable, nothing acknowledged it, and the
effect table's primary key `(at, id)`, with `at` set at ingest, would accept a resent effect as a
new row.

**Every record a node originates is written first to a durable local journal**, numbered by a
per-node sequence:

```
JournalRecord
  node          NodeID
  seq           int            monotonic per node, never reused, survives agent restart
  kind          effect | metering | break_glass | lease_released
  occurred_at   the node's clock, informational
  body          the record
```

- **The agent writes the record durably before it acts on its consequence** where an order is
  required (a break-glass session is journalled before the shell opens), and immediately after the
  event otherwise.
- **The control plane ingests in sequence order.** It accepts record `seq` only if it is one past
  the highest it has ingested from that node, and it advances that cursor in the same transaction as
  the rows the record produces. A record at or below the cursor is a duplicate and is dropped; a
  record beyond the next is not acknowledged, and the node sends it again with what was missing,
  because its journal keeps everything past the cursor. So node-originated effects are ingested
  **exactly once, in order**, without asking a partitioned table for a unique index it cannot have.

  > Corrected 2026-10-13, from the implementation. This said a record beyond the next "is held
  > until the gap fills". The node's journal already holds it, durably, and resends everything
  > the cursor has not passed; a second copy held in the control plane's memory would be lost
  > with the process and could only ever duplicate what the node keeps. The control plane keeps
  > the cursor and nothing else.
- **A session begins with the cursor.** After the `Welcome`, the control plane sends a
  `JournalAck` naming the highest sequence it has ingested from the node, so an agent that
  reconnects knows where to resend from without guessing.
- **A record is believed only as far as the node's assignments reach.** An effect for a workload
  the node holds no assignment or placement for, or for a step that is not one of the node's steps
  of that workload's plan, moves nothing; it is kept as a `journal.rejected` effect naming why, and
  the cursor passes it, so one bad record cannot block a journal. A record that cannot be read as
  one at all (an identifier that does not parse, a gap misnumbered) is kept the same way, keeping
  its number, with the reason it could not be read. A believed effect is recorded with
  the control plane as its actor (`system:reconciler`, §3.2 of `01-model.md`), since the node
  executed a decision already made, and with the grant the plan was approved under. An effect
  outside any plan (an adoption, a crash restart) names the grant of the plan most recently
  approved for that workload that is applying or applied, because the decision the node is
  keeping is that one; the control plane looks it up at ingestion, where the plans are. A step's
  completion moves the plan's progress; the last step applies the plan, a failure fails it, and a
  retire plan applied retires the workload (§4.8). A failure the node marks `step.refused`,
  its driver refusing before anything of the workload was made there, places the workload
  elsewhere instead, the plan still open (§2.1 of `05-scheduling.md`). A gap record and a
  break-glass record are effects in their own right. A metering sample believed is a record of its
  own, measured from the workload's sample before it on that node (§2 of `15-observability.md`),
  and writes no effect, since metering is not the audit log; a sample not believed is a
  `journal.rejected` effect like any other record. Corrected 2026-10-21, when metering was
  stored: until then a sample was recorded only as having arrived (`journal.unsupported`).
- **`JournalAck`** tells the agent the cursor. Nothing is discarded before it is acknowledged, and
  acknowledged records are kept for a further retention window (default seven days) before they
  are discarded, so that a control plane restored from an older backup can re-ingest them (§8 of
  `03-state.md`). The window is part of the journal's declared bound. A cursor lower than the one
  the node last held means a restored control plane: what it has not ingested is no longer
  acknowledged, and is kept until it is. A cursor above everything the node has numbered means
  the node's own state is older than the control plane's (a node disk restored from a backup):
  the agent numbers its next record after the cursor, so nothing new is dropped as a duplicate,
  and whatever it held at or below the cursor is treated as ingested. That is a loss, and it is
  stated here rather than hidden.
- **A gap record stands for the numbers it names.** Its own sequence number is the last of them,
  and the control plane admits it when the first is no more than one past the cursor, so a gap
  fills a hole instead of waiting behind it. There are three causes. `journal exhausted` is the
  bound below. `discarded before the control plane ingested it` answers a session whose cursor is
  older than the journal's tail, which is how a control plane restored from a backup older than
  the retention window learns what it will never receive (§8 of `03-state.md`). `journal damaged`
  covers numbers a segment no longer reads. The last two are made as the journal is sent, because
  the hole is only a hole relative to the cursor of the session that meets it.
- **The journal is bounded, and the bound is declared.** A node partitioned for long enough fills
  it. Repeated identical events (a crash loop's restarts) are coalesced into one record with a count
  first; if the journal still fills, the agent writes a **gap record** naming the sequence range
  and the cause, exactly as streams and metering do (§3 of `14-streams.md`, §2 of
  `15-observability.md`). Workloads are never stopped to preserve the journal, because that would
  convert a full disk into an outage, and the loss is visible rather than silent.

  > Added 2026-10-13, from the implementation: the order in which the bound is defended, and what
  > coalescing costs. Coalescing comes first: the first of a run of identical effects is written at
  > once, and repeats within a window (default ten seconds) are counted and written as one record
  > with the count when the window passes or something else is recorded. Only effects coalesce; a
  > record whose order matters (a break-glass session, a lease release) is always on disk before
  > the append returns. The cost is that a count held in memory dies with the agent process, so a
  > crash can understate a run by one window's repeats; it never loses a distinct event. If the
  > journal still fills, acknowledged records still inside their retention window are discarded
  > next, oldest segment first, because losing what a restored control plane might re-ingest is a
  > smaller loss than losing what no control plane has seen; so the window is kept only while the
  > bound allows it. Only then are records lost. Each lost record keeps its number, the range is
  > kept durably beside the journal so a restart does not forget it, and its gap record is written
  > as soon as there is room, from space reserved for it.

**On disk** the journal is a directory of segment files (16 MiB by default, the unit of
discarding), each record framed with its length and a CRC-32C and synced before the append
returns. A record cut short by a crash at the end of the newest segment is cut away when the agent
starts; a record that fails its checksum in an older segment ends what that segment is believed to
hold, and the numbers after it go out as a `journal damaged` gap. The sender keeps a bounded number
of records in flight ahead of the cursor, so a long backlog is paced by the control plane's
ingestion rather than pushed down the stream at once, and it sends again from the cursor if the
acknowledgements stop while records are outstanding.

The bound is the node-local buffering horizon that §7 of `15-observability.md` left open. The
default is 2 GiB per node, configurable. The horizon it buys depends on how busy the node is, so it
is not stated as a duration; the agent measures its own journal growth rate and reports the
resulting horizon in `NodeStatus`, so an operator can see how long each node can be cut off before
audit and metering have holes, rather than learn it afterwards.

---

## 5. Inside the control plane

Components, each with one responsibility and a defined interface. Nothing here reaches around
another component.

| Component | Responsibility |
|---|---|
| **Gateway** | Terminates all client connections. Authenticates the subject. Resolves and evaluates grants. Rejects anything unauthorized before it reaches the model. The single authorization path (P6). |
| **Planner** | Given a proposed `Intent`, computes a `Plan`: the steps, their reversibility, and the impact assessment. Pure, it reads state and produces a plan; it changes nothing. |
| **Scheduler** | Chooses a node for a workload, honouring constraints, quotas, driver capabilities, and placement policy. Records *why*. Detail in `05-scheduling.md`. |
| **Dispatcher** | Owns the agents' control streams. Publishes assignments and answers lease requests, ingests observations and journal records. Stateless, any replica can serve any agent. |
| **Store** | The authoritative state. Owns intents, plans, grants, and the effect log. Detail in `03-state.md`. |
| **Relay** | Joins client sessions to nodes' outbound data connections, verifying capability tokens offline (§3.1). Holds no state. Persists nothing: stream segments live on the node and offload from there (§4 of `14-streams.md`). |
| **Content** | Resolves recipe references to digests, verifies signatures, tells nodes what to prefetch. Detail in `09-recipes.md`. |

The Planner being pure is deliberate: it makes dry-run free, makes plans testable without a running
cluster, and prevents the class of bug where computing a plan has side effects.

### 5.1 What "resolves and evaluates grants" costs

> External review, finding R5. Recorded in §16 of `23-walkthroughs.md`.

One table row is not a design. Authorization here is a search for a permitting chain, and the
search is not small: a subject may hold grants directly and through N external identities, each
chain is d links deep, each link's validity is derived rather than stored (§3.5 of
`08-identity.md`), and a selector scope has to be evaluated against the current workload set on top
of that. Worst case is O(N × d) reads plus selector resolution, **per request**, and Bet 3 puts an
authorization in front of every plan computation, which is every click.

Caching is the obvious answer and it collides with the promise directly above it: revocation is
immediate. A cache with no invalidation design turns "immediate" into "eventually, by TTL", which
is the same class of claim this project refuses everywhere else.

So the closure is materialized and invalidated by event, not by clock:

| | |
|---|---|
| **What is materialized** | `subject → (action, scope, conditions)` closure, per subject, derived from the chains, each entry carrying the set of grant and binding identifiers its chains pass through |
| **What invalidates it** | a grant issued, revoked, expired, or suspended; a binding changing state (§5.1 of `08-identity.md`); a selector's membership changing |
| **How a change is recorded** | the transaction that makes it increments a single store-wide `authz_version` and appends one `authz_change` row naming the identifiers affected |
| **How a gateway learns of it** | from the snapshot every request already reads (below); `NOTIFY` on commit is a prefetch hint and nothing depends on it |
| **What happens on a miss** | full chain evaluation, which is the correct answer and is what a cold gateway always does |
| **What is never cached** | `max_uses` redemption (§6.2 of `08-identity.md`), `requires_mfa`, `requires_approval_by` |

> Second external review, finding S10. Recorded in §17 of `23-walkthroughs.md`. The first version
> carried invalidations on the job queue of §6 of `03-state.md`. That queue hands each job to
> exactly one worker (`FOR UPDATE SKIP LOCKED`), so it cannot tell every gateway replica anything.
> It also required invalidating every subject whose closure passed through a descendant of a revoked
> grant, which is the subtree walk that §3.5 of `08-identity.md` (finding R4) had removed from
> revocation, moved into the revoking transaction instead. And "a gateway serving a stale closure
> refuses" needed a store read on every request to know it was stale, which the table did not
> account for.

**Authorization is evaluated against the snapshot the request reads.** Every API request opens a
read transaction, because it reads the objects it acts on. The gateway reads `authz_version` in that
same snapshot. If its cached closure was computed at that version, the closure is exact for this
request, with no staleness window at all. If the version has moved, the gateway reads the
`authz_change` rows between the two versions, drops every cached entry whose identifier set
contains an affected grant or binding, and re-evaluates only those. Revoking a grant therefore costs
one revocation row and one change row, as R4 intended: the descendants of a revoked grant are
dropped because their chains contain its identifier, without anyone walking the subtree to find
them.

So revocation is **immediate** in the precise sense: no request whose snapshot includes the
revocation is authorized by a closure that predates it. A gateway that missed every `NOTIFY` is not
stale; it is merely colder. `authz_change` rows are retained long enough to cover the oldest closure
any gateway may hold, and a gateway whose cache is older than that retention discards it whole.

**The cost, stated.** One extra indexed single-row read inside a transaction the request opens
anyway, plus a range read when authority has changed since the last request. When the store is
unreachable no request can be authorized, so the API fails closed rather than authorizing from
memory; established relay sessions continue on their tokens (§3.1).

Selector scopes are the expensive case and they were already flagged as dangerous for a different
reason (§3.4 of `08-identity.md`, the set changes after the grant is issued). Both problems have
one answer: a selector grant's membership is materialized alongside the closure, so a workload
gaining a label is an invalidation event rather than a silent widening nobody evaluated.

The gateway is stateless and horizontally scaled with no leader (§5 of `18-operations.md`), so this
is a throughput cost rather than a bottleneck. It is stated because a design that puts one
authorization on the path of every operation owes a number, and "it evaluates grants" was not one.

---

## 6. Inside the agent

The agent is where Rule K-1 and P3 are either honoured or lost. The largest single class of
published Wings advisories (§1.1 of `research/evidence.md`) is a privileged process performing
filesystem operations on tenant-supplied paths, defending itself by inspecting path strings. The
structure below is designed so that class of bug has nowhere to live.

```
┌──────────────────────────────────────────────────────────────┐
│  Supervisor            unprivileged user, no CAP_*           │
│  Holds the control stream and the data connection. Runs the  │
│  reconciliation loop. Computes steps. Never touches tenant   │
│  files.                                                      │
└───────┬───────────────────────┬──────────────────────────────┘
        │                       │
        │ unix socket           │ unix socket
        ▼                       ▼
┌──────────────────┐   ┌────────────────────────────────────────┐
│  Privileged      │   │  Runtime drivers                       │
│  helper          │   │  oci · native · firecracker · qemu ·   │
│  root, tiny,     │   │  windows. Each declares its            │
│  socket-         │   │  capabilities and validates every spec │
│  activated       │   │  it receives (below). 04-runtimes.md.  │
│                  │   └────────────────────────────────────────┘
│  Verbs only:     │
│  mount, unmount, │   ┌────────────────────────────────────────┐
│  cgroup write,   │   │  Filesystem workers. ONE PER WORKLOAD  │
│  netns create,   │   │  In a mount namespace holding that     │
│  volume create,  │   │  workload's volumes and nothing else.  │
│  snapshot,       │   │  Unprivileged. Landlock-confined.      │
│  worker spawn,   │   │  openat2(RESOLVE_BENEATH) for every    │
│  job start       │   │  path operation.                       │
│  No paths from   │   │  Serves the file manager and SFTP.     │
│  tenants. No     │   │  Cannot name a path outside the        │
│  arbitrary exec. │   │  workload's volumes, not by policy, by │
│                  │   │  kernel.                               │
└──────────────────┘   └────────────────────────────────────────┘
```

**The ingress proxy is a fourth kind of process.** Added 2026-10-15, with the HTTP ingress (§3.2 of
`07-networking.md`). `korpis-ingress` is the one process that parses requests from the internet, so
it is held to the filesystem workers' rule from the other side: it accepts untrusted input and
holds no privilege, no filesystem, and nothing of the agent's. The supervisor has the helper start
it, as the supervisor's own user in a cgroup of its own whose new connections may leave only
through a workload's link (§3.2 of `07-networking.md`), hands it the sockets the helper opened on
ports 80 and 443 and its routes and certificates over a Unix socket (§3.2 of `07-networking.md`),
and restarts it if it exits.

**The reconciler never waits in a stop.** Added 2026-10-19, before the recipe's `pre_stop` hook
was built (§3 of `09-recipes.md`). The supervisor's reconciler moves through the node's workloads
one after another, and a stop it waited in held up every other workload on the node for as long:
thirty seconds for an object that ignores its terminating signal, and a hook allowed ten minutes
would have made it ten. A stop, of a workload's object or a run's, is instead a state kept with
the object: each pass moves it on (the object is signalled, then killed once its grace is over,
thirty seconds) and says whether it has stopped, and whatever needed it stopped, a power stop, a
restart, a rebuild, a retirement and the lease's release after it, a supersession, a run's cancel,
replacement, or timeout, carries on in the pass that finds it so; a plan step still stopping is
left for a later pass, as a backup's is. The process's exit is a change the driver reports, which
wakes the reconciler at once; the kill comes in the first pass after the grace, at most one
period late (two seconds by default). What it costs: a stop is no longer one call to the driver
but a signal, a kill, and a look on each pass, and an agent that restarts in the middle of one
forgets how far it was and signals the object again, which starts its grace again.

> Corrected 2026-10-24. A signal the driver could not deliver failed the pass and was asked
> again on the next, so the object was never killed: a microVM whose Firecracker had stopped
> answering, the hung guest a liveness probe exists to restart, could be sent neither its stop
> through its agent nor Ctrl+Alt+Del, and stayed. A signal that cannot be delivered is now said
> and not asked again, and the stop goes on to its kill at the grace's end as for an object that
> ignored it; the `firecracker` driver's own `Stop` schedules its kill before it signals, for the
> same reason. What it costs: an object the driver cannot reach for a moment is not signalled
> again once the moment passes, and is killed at the grace's end instead of being told twice.

> Added 2026-10-20. A retirement's last call, destroying the object, is left for a later pass
> in the same way when the driver does not answer it in time or cannot be reached, rather than
> failing the retirement: nothing then says the object is not gone or going, and destroying it
> again is harmless, since an object already gone counts as destroyed. A full check failed a
> retirement on a loaded machine whose containerd took longer than the call's thirty seconds to
> remove a container, and the workload was left neither running nor retired. What it costs: a
> driver that never answers leaves the retirement waiting rather than failed, and the plan says
> so only by not finishing.

> Corrected 2026-10-24 (Finding 165 of `23-walkthroughs.md`). The 2026-10-20 note left the
> destroy for a later pass and not the stop before it: asking whether the object still ran, and
> killing it at the grace's end, failed the stop when the driver did not answer in time, and with
> it the retirement, which the note above had meant to keep from failing. containerd answers a
> task's state late while its shim is busy, which a loaded machine makes common. Each is now asked
> again on the next pass in the same way: an object whose state was not answered is not known to
> have stopped, so its stop goes on, and a kill not answered is sent again. What it costs: the
> same as above, a driver that never answers leaves the stop waiting rather than failed.

Three properties follow from this structure:

**No component both holds privilege and accepts tenant input.** The supervisor accepts
control-plane input and holds no privilege. The privileged helper holds privilege and accepts only
a fixed verb set with structured arguments, never a tenant-supplied path. The filesystem workers
accept tenant input and hold no privilege, and are confined to a subtree by the kernel, so a
traversal bug produces `EXDEV`, not a host file.

**File operations happen inside a namespace that contains only the files they may touch.**
Pterodactyl's Wings performs file operations for every tenant from one privileged process, which is
why a symlink race in one server's directory can reach the host. In Korpis, the helper's `worker
spawn` verb creates a fresh mount namespace, mounts into it the volumes of exactly one workload
(named by volume identifier, never by path), applies the Landlock ruleset, and starts the worker
inside it. There is no host path for the worker to reach, because the host filesystem is not in its
namespace.

> As built, 2026-10-15, for installs: the helper's verb is `StartJob` with install work, and the
> worker is `korpis-install-job`, running as the workload's own ids in a user namespace with its
> volumes idmapped into it (§4 of `09-recipes.md`). As built, 2026-10-17, for file sessions: the
> verb is `OpenFiles`, and the worker is `korpis-fsworker`, built on the same root and the same
> confinement (`korpis_confine::root`), started for one session and gone when the session's
> socket closes rather than socket-activated and idle-timed, which is the same cost bound: a
> process per workload whose files someone has open (§3.2 of `06-storage.md`).

> Second external review, finding S79. Recorded in §17 of `23-walkthroughs.md`. The first version
> ran one worker per **tenant**, inside "that tenant's mount namespace". A tenant has no single
> mount namespace; each of its workloads has one, and a stopped container has none at all, which is
> exactly when people edit configuration files. A worker spanning a tenant's workloads would also
> have to decide, by inspecting paths, that a share link scoped to one server may not reach its
> siblings, which is the defence by string that K-1 forbids. One worker per workload, in a namespace
> built for it, makes that decision the kernel's. Workers are socket-activated and exit when idle,
> so the cost is a process per workload in active use, not per workload in existence.

**Each workload has an identity socket, and the supervisor serves it.** The supervisor creates one
Unix socket per workload it runs, in a directory of that workload's own under its identity root,
and hands the directory to the driver, which mounts it read-only at `/run/korpis` inside that
workload and nowhere else; the socket is `/run/korpis/identity.sock`. A request on it is forwarded
as a `CredentialRequest` naming the workload the socket was created for; the request carries
nothing the workload chose about who it is. The control plane decides whether to answer (§9.5 of
`08-identity.md`). The supervisor holds no credential afterwards: it passes the answer back,
together with the control plane's address and the CA it pinned, and keeps nothing. The directory of
an installed extension, or of a workload holding a grant in force, also holds `api.sock`, which the
supervisor relays to the control plane's API (§9.5 of `08-identity.md`; corrected 2026-10-19,
Finding 103 of `23-walkthroughs.md`, when it was an installed extension's alone). The same
directory carries the workload's managed files in `config/`, rendered by the supervisor (§5.2 of
`09-recipes.md`), so `/run/korpis` is everything the node gives a workload that the workload may
read and not write. A driver says whether it gives a workload the directory (`identity_dir`, §4.1
of `04-runtimes.md`); the `firecracker` driver did not until 2026-10-24, since a guest's
filesystem is its own and nothing carried the directory into it before the vsock channel of Phase
3 (Finding 154 of `23-walkthroughs.md`). That channel now carries it: the identity socket and the
API relay are served in the guest by its first process at the same paths, and the managed files
and the dependencies' files are sent as copies as they change, so the `firecracker` driver
declares it too (§6 of `04-runtimes.md`).

The directory is mounted rather than the socket because a supervisor that restarts binds the
socket again, as a new file, and a mount of the old file would keep pointing at nothing; the
container keeps the directory and finds the new socket in it. On the host, the identity root is
readable by the supervisor's user and group alone, so no other local user can connect to a
workload's socket and obtain its credential. Anyone who can act as the supervisor can, which is no
more than the supervisor already holds.

> Added 2026-10-13, with the workload identity of §9.5 of `08-identity.md`. The directory mount
> and the root's mode were settled when the socket was built.

**SFTP and the file manager are the same worker.** They are the two highest-risk surfaces in any
panel, arbitrary paths, arbitrary names, arbitrary sizes, from untrusted users. Giving them one
confined implementation instead of two privileged ones removes the most productive bug source in
this product category.

> Phased 2026-10-16. The file manager is Phase 1's, with the web client that is its first user
> (§3.2 of `06-storage.md`); SFTP had no phase until then and is Phase 2's (§4 of
> `20-roadmap.md`), where the tenants who need it without a shell first exist. When it is built it
> is a second protocol in front of the same worker and the same file operations, so nothing about
> the worker is decided twice.
>
> Designed 2026-10-22: SFTP is served by a relay (§3.1), which speaks the file session to the
> node like any other client, so the worker on the node is the file manager's, unchanged.

**Where the volume is a block device the guest owns, the worker runs in a guest kernel.** A
`microvm` or `vm` volume is formatted and mounted by the guest (§3.1 of `06-storage.md`), so the
host cannot mount it while the guest runs, and mounting a filesystem image a hostile tenant
formatted would hand the host kernel's filesystem parser to that tenant. §4.4 of `04-runtimes.md`
gives the mechanism: the worker runs inside the workload's own guest while it is running, and inside
a disposable service microVM when it is not. The host never parses a tenant-formatted filesystem.

### 6.1 A driver is a privilege, and it guards its own door

> Second external review, finding S54. Recorded in §17 of `23-walkthroughs.md`.

The diagram makes the supervisor unprivileged, and it is, in the sense that it holds no capability
itself. What it can **cause** is bounded by what the drivers will do when asked, and the `oci`
driver holds the largest privilege on the node: access to containerd's socket is equivalent to root,
because containerd will create a privileged container with the host's root filesystem mounted if a
client asks for one. The first version of this section did not say so, and left the impression that
compromising the supervisor yielded an unprivileged process.

So the boundary is drawn at each driver, not at the supervisor:

- **Every driver validates every spec against the invariants of its tier**, independently of the
  supervisor, and refuses anything outside them: for `container`, user-namespace remapping,
  the seccomp profile, `no-new-privileges`, a read-only root filesystem, the dropped capability set,
  and no mount other than the volume mounts the helper prepared, the workload's own identity
  directory, read-only and only the one under the identity root that bears the workload's
  identifier (§2 of `04-runtimes.md`), and what the node's Container Device Interface files give
  a device the spec names, which the driver reads itself and never takes from the supervisor
  (§4.2 there; added 2026-10-25, Finding 169 of `23-walkthroughs.md`). A spec
  the supervisor could not have produced from an honest assignment is refused, logged, and reported
  as a security event.
- **Each driver runs as its own user with access only to what its runtime needs**, so the union of
  driver privileges never lands in one process (§3 of `04-runtimes.md`).
- **Only the supervisor may ask.** A driver's socket opens to its owner and group alone, and each
  connection's peer credentials, which the kernel records at `connect` and no client can state,
  must name a user the driver was told to allow, or the connection is closed before a byte of it
  is read. What this does not stop is root on the node, which can open containerd's socket
  without asking the driver at all.
- **Driver validation code is held to the same review standard as the helper**, because for the
  `oci` driver it is the only thing between a compromised supervisor and root. §6 of `21-stack.md`
  records what this meant for the language choice, and why it no longer splits it.

The honest statement is therefore: a compromised supervisor can ask for anything and obtain only
what an honest assignment could have produced. One part of that is narrower than it reads: the
driver checks a device against the host's files and not against the placements, which it does not
know, so a compromised supervisor can give one of the node's workloads a device another holds; that
stays within the node's tenants, which a compromised node already reaches (§9 of `17-security.md`),
and never reaches a host path or a program of the supervisor's choosing. It does not reduce the oci
driver's own attack surface, which is containerd's, and §6 of `17-security.md` carries that
negative.

### 6.2 The agent is several executables, and their protocol is versioned too

> Second external review, finding S55.

The supervisor, the helper, the filesystem workers, the ingress proxy, and each driver are separate
executables connected by local sockets. Those sockets carry a protocol, and the first version of
this document versioned the protocol between the agent and the control plane while saying nothing
about the one inside the agent, so an upgrade that replaced the supervisor but not the root helper
had no defined behaviour.

- **The internal protocols are protobuf schemas in the same published tree as the agent protocol**
  (§11 of `10-api.md`), governed by the same compatibility rules (§4 of `10-api.md`). A driver
  written by a third party conforms to the same contract as a first-party one (K-7).
- **The executables ship as one signed package and are upgraded together**, but no ordering is
  assumed inside a node: the supervisor and the helper each support the other's current and
  previous internal version, so a supervisor that restarts first talks to the old helper until the
  helper's next socket activation replaces it.
- **The helper's transport is not HTTP.** The helper is synchronous and single-threaded (§4 of
  `21-stack.md`), and an HTTP/2 server is the code it should least carry. Its schema names the
  verbs, and two envelopes carry them on a Unix stream socket: each message is a 4-byte
  big-endian length and one `HelperRequest` or `HelperResponse`, at most 1 MiB, one request at a
  time. A verb the helper does not know is answered as unsupported rather than misread, which is
  what lets a newer supervisor talk to an older helper. The socket is systemd's, passed to the
  helper as standard input when a connection arrives, so the one root process runs only while it
  is asked something and exits when idle; the supervisor connects once per call. Each connection's
  peer credentials must name the supervisor's user before a byte of it is read, as for a driver
  (§6.1).
- **Filesystem workers hold no state**, so a running worker finishes its sessions on the old
  version and its successor starts on the new one. Drivers are upgraded one at a time and re-adopt
  their runtime objects through `Recover` (§4.2 of `04-runtimes.md`), which is the same path an
  agent restart already takes.

---

## 7. Transport, identity, and versioning

### Transport

Two connections over TLS 1.3, both dialled out by the agent: the control stream to the control
plane (§4.2) and the data connection to each of its relays (§3.1), the latter multiplexing one
flow-controlled stream per session so that one slow reader cannot stall another. Message framing is
a versioned binary schema, normative definition in `10-api.md`.

### Node identity

An agent authenticates with a per-node key established at enrolment. Enrolment uses a single-use,
short-lived token that is itself an attenuated grant (§3.2 of `01-model.md`), issuing a node token
is not a special code path, it is `node.enroll` scoped to one organization with `max_uses: 1`.
The node belongs to that organization from then on, and serves it and every organization below it
(§3.4 of `01-model.md`).

The node generates its Ed25519 identity key itself and sends only the public half at enrolment, so
the private key never leaves the machine. Every session then runs over TLS to the control-plane
certificate the agent pinned at enrolment, and its first message proves the other direction: the
`Hello` carries a signature by the node key over the node identifier, a timestamp, and a nonce,
which the control plane checks against the enrolled public key, a declared clock-skew window, and
the nonces it has seen inside that window (`NodeAuth` in `korpis/agent/v1/agent.proto`).

The window is five minutes either side of the control plane's clock, and a nonce is remembered for
twice that, so a captured Hello cannot open a second session while its timestamp would still be
accepted. Every failure of the proof is answered `UNAUTHENTICATED` with nothing more, except a
timestamp outside the window, which says so: it is the one refusal an honest agent can act on, by
its clock, and `Welcome` carries the control plane's time so the agent can notice drift before its
next Hello is refused. A decommissioned node's key opens nothing, and the session it held when it
was removed ends at once, every replica telling its sessions to look again. A node holds one
session; a new one replaces the old, whose stream ends, so an agent that reconnected after a
network change does not leave a half-open twin receiving its assignments. The node's labels are the
operator's, bound at enrollment, and a `Hello` that reports others changes nothing: a node that
could label itself could attract the workloads of anyone whose placement selects on the label.

**No node ever receives a credential it does not need.** The critical advisory GHSA-pfvc-3p5h-x7h6
exists because Wings held registry credentials and a daemon token in a scope that tenant-authored
templates could reach. In Korpis, registry credentials are held by the Content component and
exchanged for **short-lived, digest-scoped pull tokens** issued per fetch. A compromised node
yields a token that can pull one already-known artifact for a few minutes.

The same rule governs backup and stream offload. A node never holds a repository's credentials or
its key-encryption key. For each backup job it receives the data key of the one workload being
backed up and a short-lived credential that can write, read, and list objects under that workload's
prefix, delete none, and reach nothing else; where a store cannot issue such a credential, its
provider serves it and issues one of its own (§5.5 of `06-storage.md`; corrected 2026-10-15, when
this said the upload was brokered through a relay). A compromised node therefore reaches the
backups of the workloads placed on it, which it could already read live, and no others.

### Protocol versioning (Rule K-7)

- The protocol is versioned independently of both the control plane and the agent.
- The agent declares its protocol version and driver capabilities in `Hello`. The control plane
  never infers capability from a version number, it asks, following Nomad's task driver interface.
- The control plane supports the current protocol version and the two preceding ones. Support
  windows are published, not discovered.
- Upgrade order is always: control plane first, then agents, one at a time. A mixed fleet is a
  supported state, not an emergency.
- The protocol definition is Apache-2.0 and published separately from the implementation, so that a
  third-party agent is a normal thing to build rather than a reverse-engineering exercise.

---

## 8. High availability

**The control plane is stateless.** Every replica can serve any client and any agent. Agents
connect through a load balancer and reconnect freely; there is no affinity between an agent and a
replica.

**State lives in the store**, which is the only stateful component and the only thing requiring a
replication strategy. Detail in `03-state.md`.

**Leases are held in the store with fencing epochs**, so replica failover cannot produce two
authorized writers for one workload. Exclusivity rests on the lease rule of §4.5, which needs no
coordination layer, and where the storage can refuse a superseded writer the epoch is enforced
there as well (`fencing: storage_enforced`, §2 of `06-storage.md`).

The honest framing: control plane HA matters **less** here than in comparable systems, because §4.6
guarantees the data plane survives a total control plane outage. Losing the control plane costs
management, not availability. That is a deliberate consequence of P1, and it means HA can be added
when it is needed rather than being a precondition for the first release.

---

## 9. Failure modes

Each row is a state the system must be able to represent and recover from without manual database
surgery, the standard Pterodactyl fails (issues #4505, #3332).

| Failure | Behaviour |
|---|---|
| Agent process dies | Workloads keep running. On restart the agent adopts them (§4.4) and resumes delivering its journal from the cursor the control plane names when the session opens (§4.10). |
| Agent host reboots | Workloads restart per their assignment. Adoption finds what systemd already restarted. |
| Network partition, node side | Agent keeps converging to its last assignments and journals what happens. Leases with `keep_running` persist; `stop` leases end at the holder's end of the term and those workloads stop. |
| Network partition, control plane side | Node marked `unreachable`. Observations go `unknown`, never a stale value shown as current. No new placements on that node. `keep_running` workloads are not rescheduled automatically (§4.5); `stop` workloads are, once the grantor's end of their lease has passed. |
| Node returns after its workloads were reassigned | Its refusals name the superseding node; it stops those workloads, keeps their data, and reports the volumes as orphans (§4.5). No split brain, and no data deleted on the node's initiative. |
| Control plane replica dies | Agents reconnect to another replica. No state is lost; the store is authoritative. |
| Relay dies | New sessions route through another relay; sessions on the dead one end and resume from their last offset (§3.1). |
| Store unavailable | The API fails closed, because no request can be authorized without a snapshot (§5.1). Agents keep converging and journalling; established relay sessions continue on their tokens. |
| A plan fails halfway | Completed steps are recorded as effects, and the resources they created are retained and named. The plan is `failed` and **resumable from its last completed step** (§3.1 of `05-scheduling.md`); it is not recomputed from scratch, because recomputing would discard the record of what was half done. |
| Migration interrupted | Source retains the lease until cutover completes and is verified. Interruption leaves the source authoritative and the destination's partial copy is garbage-collected. The workload never stops being owned by exactly one node (Rule K-4). |
| Two agents claim one workload | Impossible. One lease epoch is valid at a time, and a new epoch is issued to another node only after the old one is provably over (§4.5). |
| Agent is compromised | It holds a node key scoped to its own node, short-lived digest-scoped pull tokens, and per-job backup credentials for its own workloads. Its supervisor can request only specs its drivers will accept (§6.1). It cannot read another node's assignments, mint grants, or write effects attributed to another actor. |

> Second external review, finding S4. The "plan fails halfway" row said the next reconciliation
> computes a fresh plan and there is "no partially-applied limbo", while §3.1 of `05-scheduling.md`
> made a failed plan resumable from its last completed step and §7 of `03-state.md` computed the
> next plan against the partial state. Resumable is the stronger promise and the one P9 requires;
> this row now says the same thing as the other two.

---

## 10. Open questions

1. **Emergency console during a control plane outage. Resolved: a local socket, not a listener.**
   The framing assumed the operator reaches the agent over the network, which collides with §3, the
   agent has no inbound port, and opening one for an emergency would add permanent attack surface
   for an occasional need.

But if the control plane is down, the operator already has to reach the machine some other way, and
that way is SSH or a physical console. So break-glass is a **unix socket on the node**:
`korpis-agent console <workload>`, authorized by a break-glass token (§6 of `08-identity.md`) or by
being root on the box, with every use written to the node's journal before the session opens and
delivered to the effect log on reconnect (§4.10).

No network surface is added, nothing is cached that was not already deliberate, and the honest
limit is stated rather than hidden: **this does not work from a phone.** A mechanism that pretended
otherwise would be a listening socket on every node in the fleet, permanently, for an event that
happens twice a year.

2. **Does the agent cache grants at all? Resolved: no, it verifies tokens, and the distinction is
   the whole answer.** Settled in §6 of `08-identity.md`.

A grant is a policy that must be evaluated against a chain of parents, conditions, and scopes;
caching that on a node would mean putting the authorization engine and its inputs on the data
plane, where a compromised node could reason its way to a conclusion nobody authorized. A
capability token is the *result* of that evaluation (already performed, online, by the control
plane) signed, narrowly scoped, and short-lived.

So the agent holds no grants and evaluates nothing. It checks a signature and an expiry. Revocation
latency equals the token lifetime, which is the honest trade and the reason lifetimes are short.
3. **Full-resync cost at scale.** A node running several hundred workloads receiving the complete
   intent set on every reconnect is expensive. Content-addressed intent sets with digest comparison
   would let the agent say "I already have this exact set", but that adds a second identity concept
   to intents. **Resolved in §9 of `10-api.md`**, a set-level digest, not a per-intent identity.
4. **Multi-workload plans across nodes. Resolved in §3 of `05-scheduling.md`**: an `Operation` owns
   one plan per affected workload, generated as it proceeds. Draining a node produces migrations for
   many workloads. Is that one plan with many steps, many plans under a parent operation, or a
   distinct `Operation` object? This is question 1 of `01-model.md` and it needed settling before
   the scheduler was designed.
5. **Should the agent be able to reject an intent? Resolved in §7 of `05-scheduling.md`**: it
   reports a fact (`unsatisfiable`, with the reason) and the scheduler decides. A node may be unable
   to satisfy an intent it has been assigned, a driver is missing, hardware is absent, a volume will
   not mount. This surfaced as persistent non-convergence. An explicit `Reject` with a reason would
   let the scheduler reschedule immediately instead of waiting for a timeout, but it lets the data
   plane make a decision, which sits uneasily with §2.
6. **Observation frequency and cost. Resolved here.** Continuous streaming gives an accurate
   `since` timestamp and fast failure detection at the cost of constant traffic; on-change reporting
   with periodic heartbeats is far cheaper but blurs exactly when something changed. This was
   routed to `15-observability.md`, which never took it up (finding S18).

The blur was a false premise. `since` is the agent's own record of when the state changed, taken at
the transition on the node, so it is exact however late the report travels. What reporting
frequency actually controls is **detection latency**, and that has a separate answer: the agent
reports every state transition immediately, and the **node** sends a heartbeat (`NodeStatus`) at a
declared interval, default 30 s, carrying a digest of its workloads' current observations. While
the heartbeat arrives and the digest matches, every unchanged observation is current without being
rewritten; a workload's observation becomes `unknown` (§4.3) when its node misses three heartbeats.
Constant streaming buys nothing that on-change reporting does not already give, and a heartbeat
per workload would cost a store write per workload every interval for no information. Per node, it
costs one write per node per interval, which §9 of `03-state.md` counts.
