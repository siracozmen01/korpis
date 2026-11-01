# Relays

How console keystrokes, file transfers and stream reads reach a node that accepts no inbound
connection. Tokens are on [capability tokens](capability-tokens.md); the control stream is on
[reconciliation](reconciliation.md).

## Contents

- [Why a relay](#why-a-relay)
- [How a session is carried](#how-a-session-is-carried)
- [A relay of its own](#a-relay-of-its-own)
- [From a browser](#from-a-browser)
- [SFTP](#sftp)
- [What a relay can see](#what-a-relay-can-see)

## Why a relay

A node dials out and accepts no connection, and may sit behind CGNAT, so a browser can neither reach
it nor validate a certificate for it. Making nodes reachable would make every residential and
NAT-bound node second class; sending bulk data through the control plane would make a 40 GB world
upload a database load problem and make consoles depend on the database. So the data path has a
tier of its own.

A relay is a stateless process that joins a client connection to a node's outbound data
connection. It has no database, holds no grants, and evaluates no policy beyond verifying a
capability token.

```
client --TLS--> relay <--TLS, dialled out-- agent
   token verified offline            the same token, verified again
```

- **Every session is authorized twice, by the same token.** The gateway evaluates the chain online
  and mints a short-lived token for one workload and action; the relay verifies its signature,
  expiry, the revocation list and the client's address before routing; the agent verifies it again
  before acting. A compromised relay can observe sessions it carries but cannot invent one (except
  file sessions, if it serves SFTP).
- **The relay is the first hop that sees the client's address**, so source-range conditions are
  judged there, or at the gateway for API calls. Behind a load balancer the relay accepts the PROXY
  protocol (version 1 or 2, within ten seconds) only from the addresses `--proxy-protocol-from`
  names; a connection from one of them without a header is closed, and any other connection is its
  own client. A header is believed because the balancer is, so the balancer must write it itself.
- **Relays scale horizontally and fail independently.** Each node keeps a data connection to every
  relay and to the control plane when it serves the role, so losing a relay reroutes new sessions.
  Sessions on a failed relay end; streams resume from their last offset and uploads from the size
  already written.
- **A console survives a control plane outage for one token lifetime**, since relay and agent verify
  offline against the last keys and revocation list they received.
- **On one machine the relay is a role of `korpis-server`**, on by default (`--relay-role false`
  turns it off), so there is nothing extra to install until there is a reason to scale.

A node that is reachable and holds a certificate the client trusts may advertise a direct endpoint.
The CLI and SDKs, which pin the Korpis CA, use it; browsers use it only when the certificate is
publicly trusted. Every session also works through a relay, so a node losing reachability loses
latency, not function.

## How a session is carried

A node attaches with `DataService.Attach`, proving its key over a `DataHello` that signs a message
of its own form (`korpis.agent.v1.DataHello|...`), so it cannot be replayed as a control-stream
hello, ending with the URL the node dialled, so a hello made for one relay is good at no other.

A client mints a token through the API and receives with it a route: the node the workload is placed
on, sealed by the gateway with a key derived from the token key under a fresh nonce, so the client
cannot read it and two routes to one node do not compare equal. A tenant, or a moderator holding a
console grant, may not read the host's nodes, and the relay holds no database to look a placement
up; the route keeps the relay stateless and tells the client nothing about the host's machines, not
even which of its workloads share one. The client calls `RelayService.Open` with the token, action,
workload and route; the relay opens the route with the key its feed carries, verifies the token
offline (only a token scoped to the workload itself verifies, since a relay holds no ancestry), and
asks the node to open the session under a random 128-bit identifier, told to that node alone. The
node carries it on a stream of its own (`DataService.Carry`) with its own flow control, and a relay
buffers at most a few frames of either side, so one slow session never holds up another and nothing
is buffered to disk.

The node judges the session again from what the client sent: the token offline with its own token
keys and revocation list (never the client's address, which it does not see), the workload against
the set it holds, and the action against the handlers it runs, so an action with no handler is
refused. A wrong node refuses (`not_here`, `refused`, `unimplemented`) and the client is told why; a
node silent for ten seconds fails the open as unavailable. A route outlives a migration by at most
one token lifetime, after which its node answers `not_here` and the client mints again. A session
ends when its handler returns, with the reason delivered before the streams close, or when its token
expires unrenewed, a revocation list names its chain, or the relay refuses a renewal.

## A relay of its own

A relay is enrolled like a node, receives a certificate under the installation's authority, renews
it, and runs with `korpis-server relay run`, serving the same services from no database. It is fed
what it verifies by: `RelayControlService.Feed`, opened with a request its identity key signs over a
fresh nonce, sends the control plane's clock skew window, then the token keys with their route keys,
the signed revocation list, and every node's key, each when it changes, with heartbeats between.
The relay believes a list only once its signature verifies against the keys it was fed; it takes a
feed silent for three heartbeats as lost and opens another, and stops when the control plane refuses
it as removed. With its feed lost it verifies by the last of each it received, which bounds a missed
revocation by one token lifetime; before its first feed it admits no node.

It admits a node's hello only for its own URL, by the key the feed sent, within the skew window, and
with a nonce unseen within twice that window. Its nonce memory is its own, which is why the URL is
signed, and the control plane refuses a hello naming any enrolled relay's URL. Every control plane
replica tells the nodes it holds sessions with which relays exist and whether it serves the role
too, at session open and within about two seconds of a change; clients find the same in
`ServerInfo`. `ListRelays` says whether each relay is connected: its feed's replica records it at
most every thirty seconds, and one fed within ninety is connected.

## From a browser

No browser can stream a request while reading its response, so a browser cannot hold one `Open`
stream both ways. The relay serves the same session two further ways. `RelayService.Watch` takes
the opening message and streams the session's frames back; its first answer carries a 32-byte send
key only the watcher is told. `RelayService.Send` names the session and that key and numbers its
frames; the relay delivers them to the node in order and once, skipping frames already delivered
and refusing a gap, so a retried call after a dropped connection neither loses nor repeats a
keystroke or a chunk. A key that does not match is answered as a session this process does not hold,
so the session identifier, which nodes and logs see, is no handle on it. Each `Send` judges the
session's current token again from the address that call came from, so revocation stops what is
sent and an address-limited grant holds for every frame. A `Send` reaching a replica that does not
hold the session is refused `session.not_here` and the client reopens; console output is a durable
stream and uploads resume, so reopening loses nothing, and a balancer with affinity avoids it. A
relay answers cross-origin calls for the panel's origins (`--web-origin`) and its portals', and the
control plane lists the relays in the panel page's content security policy.

## SFTP

A relay may also accept SSH on a port of its own (`--sftp-listen`), off by default, and serve the
SFTP subsystem as a client of its own file sessions. The SSH user names the workload; the client
authenticates with an SSH key its person registered; the relay checks the client holds the key and
asks the control plane, over its fed identity, for a file token for that key's person, workload and
address. It then opens the session as any client does, translates each SFTP request into the file
session's requests, and renews before expiry. A 40 GB upload never crosses the control plane, and
SFTP adds a protocol without deciding anything about files twice.

No SSH server library hands its caller the client's signature, so the control plane believes the
relay that says a client signed. A relay serving SFTP can therefore open a file session as anyone
who registered a key, on any workload their grants reach: bounded to file actions, recorded with the
relay's name, and gained by no relay that does not serve it. An operator unwilling to extend that
trust leaves SFTP off. Each relay has its own host key (`--sftp-host-key`, made at first start),
whose fingerprint `ServerInfo` lists; relays behind one name share one key file, or clients meet a
key they have never seen.

## What a relay can see

TLS terminates at the relay, so it sees the plaintext it carries: console output, keystrokes, file
contents in transit. A relay is operator infrastructure with the same trust as the control plane,
deployed on machines the operator controls. End-to-end encryption between a browser and a node would
need every node to hold a publicly trusted certificate, which is the inbound reachability this
design exists to avoid. The cost is stated on [security](security.md).
