# Rate limits

How requests are bounded, authenticated and not, and what a refusal says. Limits bound a tenant,
never the host: an operator may raise any of them as far as they like.

## Contents

- [Two buckets that attenuation cannot multiply](#two-buckets-that-attenuation-cannot-multiply)
- [How a bucket is kept](#how-a-bucket-is-kept)
- [The expensive budget](#the-expensive-budget)
- [Defaults and refusals](#defaults-and-refusals)
- [Unauthenticated attempts](#unauthenticated-attempts)
- [Guessable credentials](#guessable-credentials)
- [Denials are coalesced](#denials-are-coalesced)
- [What it costs](#what-it-costs)

## Two buckets that attenuation cannot multiply

Limits per grant would be evadable and unfair at once: attenuating mints a child with a fresh
allowance, so anyone permitted to delegate could multiply their own rate, and a grant held by a chat
role would be one bucket for every member. So an authenticated call is charged to two buckets,
neither changing under attenuation:

| Bucket | Keyed by | Why |
|---|---|---|
| individual | the authenticated subject: the user, the token, the specific external user acting through a role, the link, the workload, the extension | one person's or credential's behaviour is bounded, and a role is not a shared allowance |
| organization | each organization the subject holds a live grant in | minting children creates no capacity, every child descending from the same organization's grants |

An address is never an authenticated caller's identity: addresses are shared and spoofable, and a
limit a tenant behind CGNAT trips because of a stranger is an outage.

**What evaluates it:** the replica receiving the request, in an interceptor every client service
passes before its handler. It authenticates the request once, which names the subject, and finds the
organizations of every grant the subject holds whose whole chain is unrevoked, unsuspended and
unexpired, in one statement cached for a minute. A token minted by attenuation adds a subject and no
organization; a subject holding grants in two organizations is charged in both, which can only
refuse sooner. The agent's and relays' services authenticate by their own proofs and are not
charged: they are the installation's machines, not tenants.

## How a bucket is kept

A bucket is a generic cell rate, a rate and a burst kept as one timestamp, the instant it would be
full again, in an unlogged PostgreSQL table, so every replica charges the same buckets and a crash
forgives what was owed rather than adding to it. One statement charges every bucket a request is
charged to, all or none, and answers how far each is from empty. A full bucket is forgotten, a
bucket kept nowhere being full.

## The expensive budget

Computing a plan (declaring, starting, stopping, restarting, placing, deleting, migrating a
workload), creating or restoring a backup, taking or rolling back to a snapshot, and installing or
upgrading an extension are also charged to a second pair of buckets at a tenth of the ordinary rate,
so a loop of declarations cannot crowd out reads. A request is charged before its handler runs, so
one its handler refuses has been charged all the same.

## Defaults and refusals

| Bucket | Default (per second, burst) | Flag of `korpis-server serve` |
|---|---|---|
| individual | 20, 200 | `--rate-individual` |
| organization | 100, 1,000 | `--rate-organization` |
| expensive individual | 2, 20 | `--rate-expensive-individual` |
| expensive organization | 10, 100 | `--rate-expensive-organization` |

A refusal is `RATE_LIMITED`, retryable, `retry_after` the moment the fullest refusing bucket admits
the request (`resource_exhausted` on the Connect wire). Every response says where the caller stands
in the IETF rate limit header fields draft: `RateLimit-Policy` names each bucket charged with its
burst (`q`) and refill seconds (`w`), and `RateLimit` its remaining requests (`r`) and seconds until
full (`t`), each policy naming its bucket furthest from full; a refusal adds `Retry-After`. A client
paces itself rather than discovering the ceiling by hitting it.

## Unauthenticated attempts

An unauthenticated attempt is every presentation of a credential not yet trusted: an API token, a
workload credential or share link presented to the API, a node's `Hello`, an enrollment token, a
relay's enrollment, an access token, a refresh credential, an invitation, each sign-in factor. Each
is checked first, and one that opens is admitted, whatever its source has done: refusing a working
credential because a stranger behind the same address failed is an outage, and refusing before
checking saves nothing, the check being one indexed lookup or one signature. Each that fails is
charged to a bucket of its source, the address or the IPv6 /64 (the client a configured load
balancer's PROXY header names), 30 a minute with a burst of 30 (`--rate-failures`); once it refuses,
a failure is answered `RATE_LIMITED` rather than `UNAUTHENTICATED`, so a client in a loop is told to
stop and for how long.

Failures are not charged to their target. These credentials are 32 random bytes or signatures by
keys the control plane never sees, which no rate of guessing finds, so slowing a target protects
nothing, and the target is whatever the attempt names: anyone could name a node in a forged `Hello`
and keep it out of its session, which is a lockout anyone can trigger against someone else.

Calls anyone may make without a credential that write something are charged to a bucket of their
source at the individual rate: every call to the account service (each ceremony writes a challenge),
the session service's `Exchange`, `SignOut`, `AuthorizeDevice` and `DeviceToken`, `GetSignInTheme`,
and the external sign-in calls, since beginning one writes a pending sign-in and calls a provider.
`GetServerInfo`, answered from memory, is charged to none.

## Guessable credentials

A password and the six-digit code beside it can be guessed, so checking first would answer a guess
whatever its source had spent, `RATE_LIMITED` only telling the guesser it was wrong. So a password
sign-in asks two buckets before checking anything, in the one statement that admits or refuses it:
its source's failures, and a bucket of the sign-in name, ten guesses then one every ninety seconds;
a sign-in that succeeds is given them back. Once either refuses, nothing is checked, the right
password included. The name's bucket is kept for any well-formed name, one naming no account
included, so neither answer nor timing says whether the account exists.

After OWASP's device cookies, a browser that has signed in to an account holds a cookie the control
plane signed, and its guesses are charged to a bucket of its own and one its account's devices
share, so failures against an account do not lock out its holder's own browsers; failures from
browsers without one are counted against the account, which past a threshold refuses such browsers
for a while. That is a denial a stranger can cause, to a holder's new browsers only, and the cost of
slowing guesses at a password at all. A passkey is never slowed, being impossible to guess. See
[authentication](authentication.md).

## Denials are coalesced

A denied request costs the caller's buckets, so probing is not free. Every denial is recorded, but a
run of identical denials (keyed by subject, action and object) is one effect: the first written at
once, so an auditor sees it as it happens, the repeats within the window (sixty seconds,
`--denial-window-secs`) counted beside the log and written as one effect with `count`, `occurred_at`
and `window_end` when the window closes, by the next repeat or a pass four times a window. The count
is in the database, so a restarted or second control plane neither loses nor doubles it, and a
client in a retry loop cannot turn the audit log into an unbounded write path.

Unauthenticated failures are recorded the same way by the actor `anonymous`, coalesced by source,
kind and target, the target being what the attempt named, a `Hello`'s node if it exists, so a forged
`Hello` is in that node's effects. A failure that would open a run opens it under its own source
only while its source's bucket and a bucket the whole installation shares (600 a minute,
`--rate-failures-recorded`) have room; otherwise it is counted under the source `*`. So the log
receives at most that bucket's worth of effects, and one per kind and node, each window, however
many addresses an attacker holds and identifiers it invents; what a flood from many addresses loses
is the attribution of the excess, never its count or target. Effects of failures naming no node are
in no tenant's view; operators read them from the log.

## What it costs

One database statement per request, serializing an organization's requests on its bucket's row for
that moment. A subject's organizations are up to a minute stale, so a new grant elsewhere is charged
a minute late and a revoked one a minute more. A subject holding grants in several organizations
spends each of their buckets on every request, so an organization granting a stranger pays for the
stranger's activity elsewhere, bounded by the stranger's own bucket and ended by revoking the grant.
A source's failures are each still checked, so the bucket bounds how they are answered, not the
work; stopping a flood by volume is the network's. An address shared by many clients shares its
failure bucket, and their failures, not their successes, slow each other's. A refused request still
writes a row of working state.
