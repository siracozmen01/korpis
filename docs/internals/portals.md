# Portals

A portal is a name an organization holds, under which the control plane serves itself, so a
reseller's customers reach the panel, sign in and hold passkeys at the reseller's own address.

## What a portal is

A `Portal` belongs to one organization and names one DNS name, which the organization must hold as
an endpoint's names are held ([ingress](ingress.md#a-name-is-routed-only-by-its-holder)). At a
portal the control plane answers what it answers at its own address (the API, the panel from
`--web-root`, and `/about/source`) on the port of `--public-url`. What differs is whom it is for:
the panel wears the portal organization's theme, sign-in offers that organization's identity
providers, a passkey registered there is bound to the portal's name, and a session begun there acts
only beneath that organization (see [authentication](authentication.md)).

A name is a portal or an endpoint's route, never both, and two portals never share one: the control
plane keeps portals' names beside routed names, under the lock verification takes, and refuses each
for the other's (`endpoint.name_portal`). A portal is never the installation's own host or a
`--web-origin`. A domain cannot be deleted or delegated away while a portal's name needs it
(`domain.portal`). Making and deleting a portal is `portal.write`, reading it `portal.read`; an
organization makes as many as it holds names for. The name's records are the holder's to publish, a
`CNAME` to the installation's host or its addresses: the control plane does not know the addresses
it is reached at, since `--public-url` may name a load balancer.

## Its certificate

A portal's TLS is `acme` unless it names a `provided` certificate of its organization covering its
name. An `acme` certificate is the portal's, issued and renewed by the same issuer, directory,
account, backoff and expiry warnings as an endpoint's. Only the proof differs: no node serves a
portal, so HTTP-01 has nowhere to be answered. The issuer proves the name by DNS where a provider of
the organization serves its zone, and otherwise by TLS-ALPN-01, which the control plane answers on
its own listener: the issuer records the challenge's digest before telling the directory to look and
withdraws it when the order ends, and a connection offering only `acme-tls/1` for a portal's name is
answered by whichever replica it reaches with a certificate made from the digest, read from the
database for that handshake, then closed. So it works on any replica behind a load balancer passing
TCP, without the replicas agreeing on anything first. It needs the control plane reached on port 443
at the name, the only port a directory validates TLS-ALPN-01 on; otherwise the portal is given a
`provided` certificate or stays `failed` with the reason.

## Serving it

Each replica holds the portals and their certificates in memory, read again when the database
announces a change to either and every minute in case one was missed, and chooses by the SNI a
client sends: a portal's name gets its portal's certificate, any other name or none the
installation's own. A portal whose certificate is not issued or not valid now gets no handshake, and
says why. A request on a portal's connection whose `Host` names another name is refused 421, so the
portal a connection proved is the one its requests are for, and a cross-origin call is answered for
the portal's own origin alone. The panel at a portal opens console and file sessions at relays as at
home: the control plane names the portals' origins to every relay in its feed (`PortalOrigins`).

**What evaluates this:** whether a name may be a portal, the control plane at creation, from its
records of verified domains, routes and portals; the certificate's proof, whichever replica the
directory reaches, from the database; the certificate a connection gets and the portal its requests
are for, the replica's listener, from the SNI and the certificate it served; the relying party,
theme, providers and a session's reach, the control plane, from that portal.

## What it costs

A portal needs the control plane's own listener to terminate its TLS: behind a load balancer
terminating TLS, the control plane sees neither the name nor the handshake, and a portal's key never
leaves it but sealed, so an installation serving portals has its load balancer pass TCP, with a
PROXY header for the client's address. Every portal's traffic is the control plane's own load. And a
portal hides nothing about the installation: it is the same control plane under another name, with
the same API, server information and source page.
