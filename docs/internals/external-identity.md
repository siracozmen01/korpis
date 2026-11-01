# External identity

How a Discord role, an OIDC group or one person at an identity provider holds Korpis authority
with no Korpis account, and what that trusts. Grants themselves are on [grants](grants.md);
signing in through a provider is on [authentication](authentication.md).

## Contents

- [Bindings](#bindings)
- [Re-verification](#re-verification)
- [The delegation ceiling](#the-delegation-ceiling)
- [Acting through a chat client](#acting-through-a-chat-client)
- [The trust boundary](#the-trust-boundary)
- [Identity providers](#identity-providers)

## Bindings

A subject is a user, a token, an external identity, a workload or an extension. An external
identity is a first-class subject, so a grant can name "the `@moderator` role in Discord guild
456" directly, and losing the role removes the authority with no deprovisioning step, because
there was no provisioning step, once something notices.

```
IdentityBinding
  provider            the identity provider extension that verifies it
  selector            guild and role, guild and user, OIDC claim and value, SAML attribute and value
  kind                individual | set
  template            for a set: what its members receive
  reverify_interval   one minute to one day; fifteen minutes when not said
  grace               at most an hour; fifteen minutes when not said
  allows_irreversible for a set: the operator's override of the default below
  state               verified | unverified, with last_verified_at
```

A set binding (a role, a group) is declared by an operator, because it decides who receives
authority. An individual binding (one Discord user, one OIDC subject) is made at that person's
first authenticated use, because it decides nothing; it gives a stable subject to someone whose
authority comes from a set or a grant issued later.

A provider is an installed extension serving the `identity` type, under a name unique among its
organization's providers, and every subject it vouches for is named by that organization and name
(`org_.../corp`). No two providers vouch for one subject, and a grant names an external identity
only at or beneath the organization its provider is installed into, which the control plane checks
at issue. A set's realm (a Discord guild, the OIDC claim or SAML attribute that names groups) never
contains a colon.

A binding is declared under `identity.binding.write` on the provider's organization and read there
under `org.read`. A subject has one live binding, and a grant names an external identity only
through it. A binding starts unverified, so a grant to it waits until its provider first confirms
it. A set binding's template is exact: every grant to the set is issued from that template at the
version the binding names, so a template edit reaches the set only through an edit of the binding.
Retiring a binding suspends what is rooted in it and frees its subject to be bound again.

Whether a binding vouches is derived at every use, never written on a grant: a chain with an
external identity on it stands only while that identity's binding is live and its last
confirmation plus interval plus grace has not passed. A binding retired, found ended or simply not
re-verified in time suspends every grant rooted in it, the children its members issued included,
at their next use or renewal, and nothing that stopped re-verifying leaves one standing.

**What it costs.** A customer who wants to give a project to a group at its reseller's provider
asks the reseller to declare the binding, because bindings live where the provider is installed.
The same person at two organizations' providers is two subjects. An edit narrows at once and widens
only forward: withdrawing the irreversible override reaches every grant at its next use, while
allowing it, or moving the binding to another template, reaches grants issued afterwards.

## Re-verification

Discord does not tell Korpis a membership ended; the role stops appearing in the next signed
interaction, and if the person never interacts again Korpis observes nothing. A console session
dies within its two-minute token anyway, but a seven-day child grant or a 24-hour share link the
person left would keep working. So a binding is re-verified on its interval, not only when its
holder happens to interact. A failed re-verification makes it unverified, which suspends every
grant rooted in it exactly as revocation cascades, but reversibly: an unreachable provider must not
silently destroy a delegation tree.

| The provider | The binding |
|---|---|
| answers that the membership ended | unverified at once |
| cannot be reached | verified until last confirmation plus interval plus grace, then unverified |

The grace is stated beside the interval, so the longest an unreachable provider can extend a
membership is a number, not "until it comes back".

**A member's delegation needs the member verified.** Confirming the `@moderator` set says nothing
about whether the moderator who minted a helper grant last week is still in it. So a grant issued
by a member exercising a set grant records `via: {set, member}`, issuance refuses one that omits or
misstates it, and it stands only while both the set binding and that member are verified. The
set's re-verification asks, in the same question, about every member holding a live grant through
it or a live session that named it: one provider call per set per interval, however many members
delegated. Members who exist but never delegated are not asked about.

**How it runs.** The control plane asks the binding's own provider (never an ancestor's provider of
the same name) over the provider's stream: for a set, whether it still exists and is the same set;
for a person, whether the provider still knows them. Every few seconds a control plane claims a
batch of due bindings, so no two control planes ask the same question. A binding is due when made,
one interval after an answer, and one minute after an unanswered call, so a provider back from an
outage restores what it vouches for within a minute. An operator can ask at once rather than wait.
Changes are `identity.binding.verified` and `identity.binding.unverified` effects.

Answers can arrive out of order, so each binding and each member record counts the answers written
to it. That a subject is still there is recorded only against the count it had when asked; that a
subject ended is recorded whatever came between, because it only moves toward unverified, and the
set is asked again within a minute in case that answer was the older. A sign-in counts as an
answer for its member. An edit of a binding's terms counts too, so an answer to the old terms is
dropped and the binding is due at once. A provider may answer that it cannot tell about a member
(an OIDC provider with no refresh token for them): that member's last confirmation is kept, to lapse
on its own. A read of a binding derives its state at the control plane's clock, so a lapsed binding
reads unverified before anything records it. A stopped loop fails closed, since a binding nobody
asks lapses anyway.

## The delegation ceiling

Re-verification depends on an interval someone configured. A second bound that no configuration
widens: a grant whose chain contains a set subject anywhere stands at most the ceiling past its
issue, whatever expiry it asked for. A moderator with a ninety-day role grant mints a child that
outlives their membership by at most the ceiling. The two fail differently: re-verification
catches an ended membership, and the ceiling catches re-verification misconfigured, unreachable or
set too long.

The ceiling is seven days unless set. An organization sets its own under `org.write`; the one in
force at a scope is the lowest along it. Below the root an organization may set at most what is in
force above it, so only the root raises it, to ninety days at most; none is under an hour, and every
change is an effect. Evaluation holds it, not only issue: a link beneath a set-subject link stands
until its issue plus the ceiling at its own scope, so a lowered ceiling reaches grants already
issued. A grant issued beneath a set has its expiry clamped to the ceiling too, so what it shows is
what is enforced. The set's own grant is not beneath a set and is not held to it. A verifier
holding a chain without its scopes' ancestry takes the default, which refuses early beneath a root
that raised it.

## Acting through a chat client

A chat client never holds a user's authority. Had it exchanged a Discord identity for a token
carrying the user's grants, the bot would hold every invoking user's full authority. Instead:

1. **Interactions arrive only by HTTP, signed.** Gateway-delivered interactions carry no signature
   and are not identity; a bot in gateway mode can read and notify, and cannot act for anyone.
2. **The identity provider verifies.** The client passes the interaction on as received; the control
   plane asks the provider over its stream; the provider checks the Ed25519 signature, rejects a
   timestamp more than five minutes off, and answers who the user is, their sets, and the
   interaction's identifier as the presentation. The control plane accepts a presentation once
   within Discord's fifteen-minute interaction lifetime, so a captured interaction cannot be
   replayed, and holds the evidence only while its call is in flight. It waits ten seconds at
   most, because a person is waiting; an unreachable provider fails closed.
3. **The client gets a ticket for one action on one object.** `AssertIdentity` names the
   provider, the evidence, the action and the object, and answers a secret good once, within a
   minute, beside that client's own credential only. The client makes the ordinary API call with
   the ticket in its `korpis-as` header; the gateway spends the ticket as it authenticates, and
   evaluates the person's grants, and those of the sets the assertion named while their bindings
   vouch, for exactly that action on that object. A second action needs a second interaction. A
   ticket keeps every API call as it is and changes only whom the gateway evaluates; an
   assert-and-act call would have been a second, untyped door into the whole API.
4. **The effect names everyone**: the person, the grant that permitted it, the provider in the
   person's key, and the client as relay.

The client relays under `identity.relay` on the provider's organization, which a chat client's
installation declares. The call comes from the client, so the person's address is unknown and no
second factor is theirs: a grant with a network condition or requiring MFA does not permit it. A
ticket may name a grant for `grant.revoke` alone, so a moderator kills a leaked link from the
channel it leaked in. What no ticket can name is refused outright: issuing a grant, minting a
token, deciding a plan, reading grants, subscribing to events, enrolling nodes or relays, each of
which acts outside the gateway's one authorization or would leave the client the person's
credential. The person's binding is made at first use, verified, unless an operator retired it,
which stays retired.

**What it costs.** Nothing ties the action the client names to the command the person typed, so a
compromised chat client can spend one interaction on another action, once, within that person's
grants, recorded as its relay; it cannot keep or reuse their authority. A ticket is spent on
authentication, so a call that then fails costs the person their interaction. Consoles and file
streams open with session tokens, which no relayed call can mint, so a chat client never holds one
open for a person. The same contract serves any chat platform whose interactions are signed; one
whose are not can notify and cannot govern.

## The trust boundary

Korpis believes Discord's assertion of who holds which role, so anyone who obtains Manage Roles in
the guild can grant themselves whatever Korpis granted that role. That is the consequence of
accepting any external identity provider, true of OIDC and SAML equally, and it is stated rather
than discovered. Two defaults follow:

- **Grants to sets and link-form grants exclude irreversible actions** (deleting a volume, rolling
  it back, deleting a snapshot, deleting a workload, releasing an address, pruning backups) unless
  an operator overrides the default for that binding. The gateway enforces it at attenuation; a
  chat client may hide what a user cannot do as a courtesy, never as the decision.
- **MFA cannot be satisfied by a Discord identity alone.** A grant requiring MFA needs a Korpis
  factor; Discord's own 2FA state is invisible to Korpis and would not be trustworthy if it were.

Routine operations (start, stop, restart, console, files) flow from roles without friction, and
destructive ones need a stronger identity or its approval. That split is what makes granting a chat
role real authority defensible.

## Identity providers

OIDC, SAML, LDAP and Discord are spoken by identity provider extensions. A provider decides who is
who, so it is in the trust base by definition, like a certificate authority. So installing one
takes authority over its whole organization, its assertions are believed only as answers up its
own authenticated stream and only for its own subjects, and it can assert identities and
memberships and do nothing else. Single sign-on is a paid tier elsewhere; here the machinery is
core, because chat identity needs the same mechanism.

**OIDC** (`korpis-idp-oidc`). Configured with an issuer, a client registered there, the claim
naming groups (`groups` by default), extra scopes, and whether to ask for offline access, through
`KORPIS_OIDC_ISSUER`, `KORPIS_OIDC_CLIENT_ID`, `KORPIS_OIDC_CLIENT_SECRET`,
`KORPIS_OIDC_GROUPS_CLAIM`, `KORPIS_OIDC_SCOPES`, `KORPIS_OIDC_OFFLINE` and
`KORPIS_OIDC_STATE_DIR`. It fetches the discovery document and keys under a deadline, refetching
keys for an unknown key identifier at most once a minute. It sends the authorization request with
the state, nonce and PKCE challenge it is given (and `prompt=consent` when asking for offline
access, without which an issuer may drop the scope), redeems the code with the verifier, and
validates the ID token by OIDC Core 3.1.3.7: an asymmetric signature (RS256, ES256 or EdDSA; `none`
and HMAC refused) by a published key, the exact issuer, the client in `aud` and `azp`, `exp` not
passed (a minute of leeway), `iat` within five minutes, and the control plane's nonce. It answers
`sub` as the person (with `iss`, the one identifier OIDC says a client may rely on), each value of
the group claim as a set, and the code's SHA-256 as the presentation. Refresh tokens are kept in its
own volume, one file per person named by the SHA-256 of their `sub`, readable by it alone.

It never acts on the issuer's initiative: a sign-in the control plane did not begin finds no state
and is refused. It reads groups only from ID tokens, never the userinfo endpoint. A refresh
answered without an ID token says the person is still known and nothing of their groups. A set's
existence is not something OIDC can answer, so a set binding stays verified while configured and
its grants lapse member by member; an issuer that drops groups past some count leaves the person in
no set, which fails closed. It is tested against an independent issuer in `conformance/oidc`.

**SAML** takes the same two calls with different evidence: an `AuthnRequest` for the redirect
binding, derived from the nonce, with the state as `RelayState`; the response posted to the control
plane's own consumer address, kept as the pending sign-in's evidence, with the challenge checked by
the control plane since SAML has no verifier; and validation of signature, `InResponseTo`, audience
and validity window, answering the `NameID` as the person and an attribute's values as sets. SAML
issues no refresh tokens, so memberships are confirmed only by sign-ins. No first-party SAML
provider is built; this is what one is built to.
