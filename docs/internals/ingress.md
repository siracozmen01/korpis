# Ingress

Name-based routing for HTTP, HTTPS and TLS with SNI, where one address serves everything, and the
certificates it serves. Coolify and Cloudron do this well for one workload shape; Pterodactyl never
had it, which is why it cannot host a web app credibly. Through an edge group, see
[edges](edges.md#ingress-through-a-group); under an organization's own name, [portals](portals.md).

## Contents

- [A name is routed only by its holder](#a-name-is-routed-only-by-its-holder)
- [A name routes to one endpoint](#a-name-routes-to-one-endpoint)
- [The proxy](#the-proxy)
- [What the proxy speaks](#what-the-proxy-speaks)
- [Certificates](#certificates)
- [Proving a name](#proving-a-name)

## A name is routed only by its holder

A shared ingress letting any tenant route any name would let one take another's traffic and, since
ACME's HTTP challenge is answered by whoever the name's traffic reaches, a certificate for it too.
So a name is held: a `Domain` belongs to one organization and covers itself and every name beneath
it, and the holder of a name is the organization whose verified domain is the most specific one
covering it. A domain is verified in one of three ways:

- **by DNS**: the control plane resolves `_korpis-verify.<name>` through public DNS (the system's
  resolvers, or `korpis-server serve --dns-resolver` where those answer from an internal view),
  uncached, and finds the token it issued;
- **by delegation**: an organization holding a verified domain creates one beneath it for a
  descendant, which is how a reseller gives a customer `customer.reseller.example` without the
  customer proving anything about a zone they do not run; the delegated domain is then the most
  specific, so the reseller no longer routes those names;
- **by attestation**: an operator vouches for it, for private names public DNS does not answer and
  the installation's own.

A verified domain may not cover or lie beneath another organization's, except one delegated beneath
its parent; a pending one blocks no one. Deleting a domain deletes those delegated beneath it.
**What evaluates this:** the control plane, at verification, from its records of every verified
domain and its own DNS answer, and again at every declaration. It does not check again later: a zone
changing hands keeps its domain verified here until deleted, and what limits the damage is that
every certificate issuance proves control anew, so the zone's new owner takes the traffic and no
certificate can be issued for the old holder's endpoint.

## A name routes to one endpoint

An `ingress` endpoint lists one to a hundred names (what one ACME certificate holds), each a plain
lowercase DNS name with no wildcard and no last label of digits alone, which would make it an IPv4
address. Its protocol is `http` or `https`, with TLS `acme` (the default) or `provided`, or `tcp`
for a TLS service routed by SNI without termination (`tls.source: passthrough`). An endpoint of any
other exposure names no domain and declares no TLS.

Two endpoints may not route one name in the installation. The control plane holds routed names in
one place keyed by name: it checks them at declaration and claims them when a plan moves the
workload's target, under the lock verification takes, checking again that the organization holds
each; a target no longer naming one, and retirement, free it. So a domain cannot be deleted, or
delegated away, while that would leave a routed name without its holder; the change is refused,
naming the name.

The workload is placed on a node that can serve the ingress, which a node measures at start and
advertises (it holds ports 80 and 443 of its addresses), and the assignment carries the endpoint's
names, TLS source and certificate. The endpoint's address is the node's advertised addresses on 443,
and on 80 for the redirect and the HTTP challenge. A workload that moves takes its routes, and its
names' records follow: a DNS provider moves them, a holder publishing by hand must; an edge group
makes ingress survive a move without records changing.

## The proxy

**The proxy is a process of its own.** It parses requests from anyone on the internet, the node's
most exposed surface, so it is not the agent: `korpis-ingress` is a program the agent supervises,
confined as a backup job is, with no filesystem, no capability, no port of its own to bind, and no
network but the sockets it is given and new connections through the node's workload links and, over
TCP, the mesh. The helper opens ports 80 and 443 over TCP and 443 over UDP on the node's advertised
addresses, which needs privilege neither the agent nor the proxy has, and passes the sockets to the
agent, which hands them to the proxy over a Unix socket as `SCM_RIGHTS`, beside its routes and
certificates, sent whole whenever they change; the proxy keeps nothing on disk. The agent has the
helper start the proxy, as the agent's user in a cgroup of its own, because the cgroup is what
confines where it connects: the node's table drops a new connection from it leaving by anything but
a workload's link or the mesh, which Landlock, naming ports and not addresses, could not express.
Where the node cannot make the cgroup or the rule, the agent says the proxy runs without that part
of its confinement.

The agent restarts a proxy that goes away, waiting longer after each quick failure; the node says it
serves the ingress while the agent holds the sockets, and says so at once rather than at its next
heartbeat. `--ingress false` makes a node serve none.

## What the proxy speaks

It accepts HTTP/1.1 and HTTP/2 over TLS 1.2 and 1.3, and HTTP/3; chooses the certificate by SNI;
redirects plain HTTP to HTTPS except ACME's challenge path; routes by `Host` among the names of the
route the connection's own name chose, refusing any other 421, since a browser reuses an HTTP/2
connection for every name its certificate covers and a wildcard certificate shared by two routes
would otherwise let one reach the other; and forwards to the workload over HTTP/1.1, carrying
WebSocket upgrades. For `https`, it forwards within TLS to a workload serving its own certificate,
which it does not verify, since no authority it holds vouches for it; that hop protects nothing the
node could not see and exists for workloads serving only HTTPS. It strips any `Forwarded` and
`X-Forwarded-*` a client sent and sets its own, so the workload reads the client only from a header
the proxy wrote (`client_address: translated`). A `passthrough` endpoint is routed by the SNI of the
client's first message and spliced to the workload unread. A client asking for a name nobody routes,
or one with no certificate yet, gets no handshake.

**Bounds.** Header size and count, the time to send them, idle time, and connections in all, where a
WebSocket on an HTTP/2 stream counts as a connection while open, since it holds one to its workload,
and an HTTP/2 connection carries at most 100 streams. An oversized head is refused 431; a connection
beyond the bound is answered 503 on port 80 and closed on 443, where there is no TLS yet to answer
in; slow or idle connections are closed. Bodies are streamed, not buffered, so a slow workload holds
its client's connection as long as it does.

**WebSocket over HTTP/2.** The proxy announces extended CONNECT (RFC 8441), and a `CONNECT` with
`:protocol websocket` is forwarded as the HTTP/1.1 upgrade the workload already understands, with a
`Sec-WebSocket-Key` the proxy makes, the client answered `200` once the workload answers `101`.

**Client certificates.** A terminated endpoint may set `TlsSpec.client_certificates` to `required`
or `optional` and `TlsSpec.client_ca` to the PEM of the authorities it trusts, at most sixteen and
64 KiB, each section required to be a certificate serving as a trust anchor, so a private key pasted
among them is refused rather than skipped and published. The proxy chooses a TLS configuration by
the name read before the handshake, so only that name's authorities verify; a route asking for
certificates has its own configuration, sessions and tickets, so a session made without a
certificate is never resumed where one is needed. The verified certificate reaches the workload as
RFC 9440 says, in `Client-Cert`, and the chain in `Client-Cert-Chain`, both stripped from every
client request. No revocation is checked, so an endpoint that must refuse a revoked certificate
sooner than its expiry issues short-lived ones or checks itself; a client refused at the handshake
sees its browser's error.

**The PROXY protocol.** A `passthrough` endpoint may declare `client_address: proxy_protocol`, and
the proxy writes a version 2 header naming the client and the address it was reached at before the
first spliced byte, TCP over IPv4 when both are IPv4 (a mapped address counting as one) and over
IPv6 otherwise, with no TLVs. It is refused on any other endpoint, and the refusal is held again
where the endpoint is stored and where the proxy reads its route. A workload told to expect the
header and not parsing it fails every connection, which is the declaration's to get right.

**HTTP/3.** The proxy serves QUIC (RFC 9000) on UDP 443 with TLS 1.3, `h3` only, and no early data,
since a replayed early request would reach the workload twice. Only routes it terminates without
client certificates are served over it; others find no certificate, and their clients keep to TCP.
Requests are forwarded as over HTTP/2, and responses over TCP carry `Alt-Svc: h3=":443"; ma=86400`
(RFC 7838). Extended CONNECT is not announced on HTTP/3, so WebSockets open over TCP. QUIC
connections count against the connection limit. A QUIC stack in userspace widens what can reach a
bug in the proxy, though not what such a bug reaches, and costs more CPU than TCP in the kernel. A
connection is not migrated when a group's holder changes.

The backend's protocol is HTTP/1.1 only: HTTP/2 or gRPC to the workload is not something an endpoint
declares yet.

## Certificates

**Certificates belong to the control plane.** An `acme` endpoint's certificate is obtained for its
names from the directory the operator configures (Let's Encrypt by default) with the installation's
account, made at the first order only if the operator agreed to the directory's terms; without it no
`acme` certificate is issued and each says why. Every order makes its own key, which leaves the
control plane only sealed and only to a node serving it. It is renewed at two thirds of its life. An
`acme` certificate is its endpoint's, made when its routes are claimed; an endpoint whose names
change is given another, and one nothing names is removed. A failed attempt is retried after five
minutes, doubling to a day, which keeps an installation inside a directory's limits on failed
validations; a certificate issued before keeps serving meanwhile.

A `provided` certificate is one its organization uploads, chain and key, named by its identifier in
`TlsSpec.secret`. At upload the control plane checks the key is the leaf's and the leaf names DNS
names and has not expired; wherever an endpoint names it, that it is the organization's, valid now,
and covers every routed name, a wildcard covering one label, at declaration and again at the claim.
It is replaced in place, keeping its identifier, with a chain still covering every name served with
it, which is how it is renewed; it cannot be deleted while served. Every issued certificate is a
`certificate.expiring` event in its organization at five sixths of its life, a warning, and critical
at seventeen eighteenths, checked hourly, so one of days and one of a year are each told in time.

Every private key is stored sealed by the control plane's TLS key, in its data directory. A node
obtains a certificate and key over its session (`CertificateRequest`), answered only to a node an
endpoint served with it is placed on; the placed endpoint names the certificate's revision, which a
renewal raises, so the node's assignment changes and it asks again. The node keeps what it serves in
its state directory, readable by the agent alone, so a restart while the control plane is
unreachable still serves, and removes a certificate once no endpoint placed there uses it. A
compromised node can take the keys of the endpoints placed on it, as it could read their memory,
which is why removing a node as compromised revokes and replaces them. The workload's status shows
each certificate's state, validity and, for one not issued, the reason, from the control plane's own
records.

## Proving a name

**By DNS** where a DNS provider of the holder's organization serves the name's zone: the
`_acme-challenge` TXT record is applied for an owner of its own (`crt_.../<n>`), with a minute's
TTL, recorded before the provider is asked and withdrawn when the order ends however it ends, so one
an issuer published before it stopped is withdrawn by the next; a certificate is not removed while
one of its records stands. A provider answers once its zone changed, which its servers may not serve
yet, so the control plane tells the directory to look once its own resolver finds the record, or
after two minutes.

**Otherwise at the node**, by HTTP-01 on port 80 or TLS-ALPN-01 (RFC 8737) on 443. The token or
digest travels in the workload's assignment, and the proxy takes it from the set its node holds
rather than the agent's reconciliation, so a long pass does not hold it back; the directory is told
to look only once every node serving the names has acknowledged a set carrying it, and two seconds
more. Which way is the issuer's choice from the certificate's own record: the way that last proved
it, HTTP-01 for one never issued, and after a failure the other way, so a node whose port 80 a
provider or firewall blocks gets its certificate on the second attempt and keeps it, with nobody
declaring that port 80 is closed, which no component could check. For TLS-ALPN-01 the proxy answers
a ClientHello offering `acme-tls/1` alone, for a name it holds a digest for, with a certificate made
from the digest and closes after the handshake; for a name it holds none for it routes as any other,
so a `passthrough` workload running its own ACME client is validated by its own answer. A node
reached on neither port at the name's address is proven neither way and stays `failed` with the
directory's reason.
