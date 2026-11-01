# Authentication

How people, pipelines and workloads prove who they are, and how long a sign-in lasts. What they may
do is on [grants](grants.md); external identities are on
[external identity](external-identity.md).

## Contents

- [Factors](#factors)
- [Accounts](#accounts)
- [The first grant and root recovery](#the-first-grant-and-root-recovery)
- [Sessions](#sessions)
- [Guessing limits](#guessing-limits)
- [Signing in through an identity provider](#signing-in-through-an-identity-provider)
- [Portals](#portals)
- [SSH keys for SFTP](#ssh-keys-for-sftp)
- [A running workload's identity](#a-running-workloads-identity)

## Factors

| Factor | Notes |
|---|---|
| passkey (WebAuthn) | the default and the recommendation: phishing-resistant, a complete sign-in alone |
| TOTP | RFC 6238, SHA-1, six digits, thirty seconds, one step of skew, each code once; second class |
| password | never a sign-in alone: always with a passkey, TOTP code or recovery code |
| recovery codes | ten, each used once, shown once at enrollment |
| OIDC, SAML, Discord | through identity providers, with their stated limits |
| node enrollment token | single-use, short-lived, itself an attenuated grant |

`requires_mfa` is satisfied by a passkey or TOTP, never by an external provider's assertion, even
when an OIDC `amr` claim says two factors were used: Korpis cannot check a provider's ceremony.

A passkey is a discoverable credential with user verification required. Passwords are stored as
Argon2id with OWASP's parameters (19 MiB, two passes, one lane), hashed off the request threads at
most one per core, and refused under twelve characters. TOTP secrets are sealed by the control
plane's vault. The relying party is Korpis's own code on Rust cryptography already in the tree,
because the established Rust WebAuthn libraries are licensed on terms this project does not take,
or maintained by one person on a cadence it cannot depend on. It accepts ES256, EdDSA and RS256 and
attestation `none`: the installation has no list of authenticator models to trust, and asking would
only identify the user's device. It is tested against 1Password's software authenticator and a
browser's virtual one.

The relying party is the control plane; its identifier is the host of `--public-url` unless
`--rp-id` names a registrable suffix, and ceremonies may come from that origin and each
`--web-origin`. Each must lie inside the identifier or the server refuses to start; without a
public URL it says at start that no passkey can be used.

## Accounts

An account is a user: an identifier, a display name its issuer gives, a sign-in name its holder
chooses (lowercase, three to sixty-four of `a-z0-9._-`, unique), and its factors. It holds no
authority by being an account; everything it may do is the grants issued to `user:<id>`.

A person gets an account by redeeming an invitation. Issuing a grant to a new user is the same call
as issuing one to a new token: it creates the account, holding the grant and no factor, and an
invitation, a 256-bit secret shown once and kept as a digest. Redeeming it in the panel registers
the first factor and voids the invitation in one transaction. Invitations expire after seven days
unless the issuer says less. An account whose invitation lapsed holds its grants and cannot be
signed in to; its issuer sees it as never joined. There is no open registration by default, since
an account with no grants can do nothing and a sign-up form is only attack surface; an operator
selling to the public turns it on (`--registration open`), and such accounts hold nothing until,
say, a billing system issues them a grant.

After every factor change, something can still sign in (a passkey or a password), and a password
has a lasting second factor beside it (a passkey or TOTP), since recovery codes run out. Each change
locks the account first, so two removals cannot race to leave it with neither. Every single-use
thing (an invitation, a TOTP step, a recovery code, a passkey counter, a challenge) is spent by a
conditional write; a challenge is taken in a transaction of its own, so a refused answer spends it.
A passkey answering with a counter that did not advance is refused and recorded as
`account.passkey_cloned`.

Changing factors (through `ProfileService`) needs a recent sign-in: the session signed in or
re-authenticated within ten minutes, or the change is refused `session.not_recent`. Without that, a
session left open on a borrowed machine, or a copied access token, could replace the factors and
keep the account (OWASP ASVS 3.7.1). A caller with no session (an API token) cannot re-authenticate
and changes no factor.

**Recovery.** Several factors per account are the norm; the panel asks for a second passkey at
enrollment. There is no email-only reset by default: an emailed link is a phishable bearer
credential that bypasses every factor; an operator may enable it, and the setting says what it
costs. An administrator recovers a member the only way the model allows: by issuing grants to the
member's newly registered identity, with the old identity's grants revoked or left to expire, both
effects. The cost: a holder who lost every browser and factor gets back in only through an
administrator, or for root through the machine.

## The first grant and root recovery

`korpis-server install` creates the root organization and the first account holding the root grant
and no factor, and prints its invitation, redeemable within an hour and only by registering a
passkey in the panel. Every grant in the installation descends from that one. An unattended install
or a test uses `--root-token`, which issues the root grant to an API token printed once and says
that this root has no second factor. One installation uses one path, never both.

Whoever is root on the control plane's machine already holds the database, the key and the
process, so recovery from a lost root grant is honest about where it comes from:
`korpis-server admin recover-root`, run locally, creates a new account holding a new root grant and
prints its invitation. It needs no network listener and no secret question. It refuses a directory
holding no installation, and records `root.recovered`, visible to every tenant in scope, naming the
account, the grant, and the operator by user id with the `USER` and `SUDO_USER` it ran with: the
user id says with what authority, the names say who claimed to run it.

## Sessions

| Credential | Lifetime | Held by |
|---|---|---|
| web session | refresh credential, rotated on every use; 12 hours idle, 7 days absolute | the browser |
| CLI session | the same, begun by the device flow, kept in the OS keychain | the user's machine |
| access token | 120 s, names the user and session, carries no authority | memory |
| capability token | 120 s, minted for what is verified offline (consoles, files) | memory |
| workload credential | 15 minutes, renewed through the node running the workload | the workload |

Signing in begins a session and sets its refresh credential, a 256-bit secret kept as a digest, in
the cookie `__Host-korpis_refresh` (`HttpOnly`, `Secure`, `SameSite=Strict`, `Path=/`, no
`Domain`), which a browser keeps only when the control plane's own name set it. A `__Secure-` cookie
scoped to a path would not do: a workload served under a sibling name could set one for the parent
domain and sign a browser in to a session of its choosing (RFC 6265 8.5, 8.6). Only `Exchange` and
`SignOut` read it. The panel exchanges it for an access token, verified by signature and expiry
alone; each call's authority is evaluated then, from the grants held then. Each exchange rotates the
refresh credential, and every rotated digest is kept for the session's life, so presenting one again
ends the session as `session.reused`, since only a copy presents it twice. Strict rotation means an
exchange whose answer was lost, retried, ends the session; so the panel exchanges from one tab at a
time under a Web Lock and shares the access token with its other tabs. A grace window would spare
that and would let a copy taken in the same moment work too. Rotating the token key signs every
browser out within 120 seconds.

Every live session is listed for its user with where and when it began, and any can be ended,
including "end all others", which deletes their refresh credentials at once; their access tokens
expire within 120 seconds. A session never outlives the grants it exercises.

**The CLI** runs the OAuth device grant (RFC 8628): `AuthorizeDevice` answers a device code and a
user code of eight characters from twenty consonants (`BCDF-GHJK`), with the panel's `/device`
page, and waits ten minutes; the CLI polls every five seconds, and a poll sooner lengthens the
interval. A signed-in user sees where the authorization was asked from and what the CLI said it was,
and approves with a recent sign-in, since it begins a session as long-lived as a browser's. An
approval names the session it was given from and is honored only while that session lasts and the
account may sign in, so ending a stolen panel session withdraws approvals it gave. Where no keychain
is reachable the CLI says so and keeps the credential in a file only its user can read;
`--no-store` keeps it in memory for one command. What this does not stop: a stranger can begin an
authorization and talk someone into approving its code; the page says what it is approving and
from where, which is all a protocol can do.

## Guessing limits

Every failed sign-in is charged to its source. A password or code can be guessed, so failures also
count against the account, following OWASP's device cookies. A browser that signed in to an account
holds `__Host-korpis_device_<account>`, signed by the token key under a purpose label of its own
and honored for 180 days. Its password failures are charged to a bucket for that device (five, then
one every three minutes) and one shared by all the account's devices (ten, then one every ninety
seconds). Without a cookie, they are charged to the sign-in name's bucket (ten, then one every
ninety seconds), kept whether or not the name exists. Buckets are asked before the password is
checked and charged before, given back on success, so simultaneous guesses cannot all pass. A
stranger can therefore keep a holder from signing in from a new browser, never from one they have
used. A passkey assertion cannot be guessed and is never slowed.

The shared bucket exists because each sign-in gives a new cookie: without it, someone who once knew
the password could keep a cookie per sign-in and guess its successor five times a quarter hour per
cookie. Changing the password or TOTP stops honoring every device cookie issued before it, and the
browser making the change gets a new one. A refusal is the same refusal whatever the reason, and a
password naming no account is checked against a stand-in hash, so neither answer nor timing says
whether a name exists.

## Signing in through an identity provider

A person signs in as the external subject their provider names, and no account is made; the
provider is their factor. Someone who also holds an account holds two subjects, and nothing links
them, since a link would let the weaker reach the other's grants.

The control plane never speaks OIDC or SAML. `BeginExternalSignIn(provider, code_challenge)` keeps
a state and nonce for ten minutes and asks the provider for the address to send the person to; they
return to the panel's `/sign-in/callback`, the one redirect URI an operator registers.
`CompleteExternalSignIn(state, code, code_verifier)` spends the pending sign-in once, checks the
verifier against the challenge, has the provider assert the code, and begins the session. The PKCE
verifier never leaves the tab that began the sign-in, so a code taken from a referrer or log is
worthless, and a sign-in an attacker began and steered into someone else's browser finds no
verifier there (RFC 9700 2.1.1, 4.7). Begin is rate-limited by source, since it costs a row and a
call before anyone proves anything.

The installation's sign-in page offers the root organization's providers; a tenant's provider is
reached by its link, `/sign-in/<organization>/<name>`, so no page lists a tenant's providers (and
its customers) to strangers. A provider vouches only at and beneath its own organization.

A sign-in names the person's sets. Each counts while the person's member record vouches: the sign-in
confirms them in every named set with a live binding, and drops them from sets it no longer names.
So a person removed from a group loses what it gave at the set's next re-verification or their own
next sign-in. The sets are kept with the session and read at each call, not written into access
tokens, which would make kilobyte headers for people in hundreds of groups. Listing a person's
grants includes those of each set vouching for them at that call. OIDC re-verifies a person by
refreshing their stored refresh token one person at a time, since identity providers treat a
rotated token presented twice as stolen; only `invalid_grant` means the person is gone, and every
other refusal (a rotated client secret, a rate limit) means it cannot tell. Without refresh tokens a
member's grace runs from their last sign-in.

A recent sign-in, where one is needed, is a sign-in again through the provider, marking the
existing session recent only if the provider named the same person. The CLI uses the device flow
unchanged. A provider that is down refuses sign-in through it, failing closed, while accounts sign
in as before.

## Portals

A portal is a name an organization holds under which the control plane serves its API and panel.
It is a relying party of its own: its identifier is the portal's whole name (never a suffix, since
without the public suffix list the control plane cannot tell which suffix is registrable), its one
origin is the portal's, and the portal comes from the connection's certificate, never from the
page. A passkey registered at a portal is offered and accepted only there; challenges record their
address; a person signing in at two addresses registers a passkey at each. WebAuthn's related-origin
requests are not used: the public file would list every reseller, browsers honor only five domains
in it, and a reseller's customers would be asked for the installation's passkey.

**A session begun at a portal acts only beneath its organization.** The portal's organization holds
its DNS, so it can serve its own page there, which sees every cookie for the name and every
password typed into it and can relay every ceremony as the person. So the session reaches no further
than what that holder answers for: every access token names the organization, the gateway refuses
any object outside it before consulting a grant, lists filter by it, and issuing, revoking or
approving outside it is refused `session.portal_confined`. A `korpis login` approved at a portal is
confined the same way.

A confined session sees and ends only the sessions begun at its portal, and manages only the
passkeys registered there. The password, TOTP and recovery codes serve every address, so a confined
session changes them only when every grant the account holds lies beneath the portal's
organization. Each shared secret records the organization beneath which the session that set it was
confined, since whoever served the portal saw it; a sign-in with a password and second factor whose
records both name organizations reaches only beneath the narrower one, is refused when they are
disjoint (their common ancestor bounds nothing), and that refusal is given only after both factors
pass, so it never confirms a guessed password. A session registers a passkey only where it is
confined exactly as its address is. The portal's sign-in page wears its organization's theme and
offers that organization's providers; each portal is one more redirect URI at each provider.

Deleting a portal ends its sessions and removes its passkeys in the same transaction, each an
effect, so nothing remains usable if the name becomes a portal again; `GetPortal` says beforehand
how many passkeys are registered there and how many accounts sign in nowhere else.

**What it costs.** Two addresses, two passkeys. A password typed at a portal is seen by whoever
serves the name and signs in at the installation's address too; a person holding grants outside the
portal's organization should sign in there with a passkey. An operator signing in at a reseller's
portal reaches only the reseller there.

## SSH keys for SFTP

A person reaches their files over SFTP with an SSH key registered to their account and nothing
else. A password over SSH would be the whole credential typed into a client that asks no second
factor; a key proves possession without crossing the wire, and every SFTP client offers one.

Accepted keys are Ed25519, ECDSA P-256 or P-384, and RSA of 3,072 to 8,192 bits (none weaker than
128-bit security, acceptable past 2030 by NIST SP 800-57; 8,192 is the most the verifier checks). A
key is added with its own signature (`ssh-keygen -Y sign -n korpis-ssh-key` over
`korpis ssh-key add` and the account id), so only its holder adds it, and only to the account the
signature names: public keys are public, and without the signature anyone could register another's
key first and receive their SFTP sessions. A key is registered once in the installation by its
SHA-256 fingerprint. Adding or removing one is a factor change needing a recent sign-in; a terminal
re-authenticates through the device flow. A key opens file sessions only: no panel, API or console.

The relay serving SFTP checks that the client holds the key and asks `AuthorizeFiles` with the
key, the workload, the client's address and any token it renews, signed by the relay's identity
key over a fresh nonce. The gateway finds the key's account, evaluates its grants for file write or
read on that workload from that address, confined beneath the organization a portal-registered key
records, mints the token, and records `workload.file.sftp_authorized` naming the account, key,
relay and address. A removed key ends its sessions at their next renewal. A compromised relay
serving SFTP can open file sessions as anyone with a key, recorded as that relay; one not serving
SFTP cannot. There is no per-account limit on keys. Pipelines use `korpis files sync` with a token
instead.

## A running workload's identity

A workload holds no long-lived secret. Its node vouches for it, and only while it runs there.

The agent serves each workload a Unix socket of its own, mounted read-only at
`/run/korpis/identity.sock`; in a microVM the guest's first process serves the same path and relays
each connection over vsock to the workload's socket on the host. A process asks it for a
credential; the agent forwards the request naming the workload the socket belongs to, and the
control plane issues one only if the workload is assigned to that node authoritatively under a
lease that has not expired. The workload is then `workload:<id>` (or `extension:<id>`) and holds
exactly the grants issued to it. The credential is 256 bits, expires after fifteen minutes, is
stored as a digest with its node and lease epoch, and is renewed by asking again, so each renewal is
a fresh proof the node still runs it. Authentication checks the workload's placement on every call,
so a move or retirement ends every credential of the old placement at once. A request before the
lease arrives is refused retryably (`credential.no_lease`, answered `unavailable`); only a workload
placed elsewhere is denied. The socket also answers `Heartbeat`, recorded on the node for the
heartbeat probe, needing no credential.

The socket answers with the control plane's address as the node reaches it and the CA the node
pinned at enrollment. A second socket, `/run/korpis/api.sock`, relays connections byte for byte to
the control plane's API over the host's network; the workload speaks TLS through it to the control
plane itself, so the agent carries what it cannot read. It is served to every workload running an
installed extension or holding a grant in force, as the control plane marks each assignment; on a
single machine the control plane is on loopback, which no workload's namespace reaches, and this
needs no exception to the egress default. A grant's own expiry is found at the next resync, so the
socket can outlive the authority briefly, refusing every call meanwhile.

**What evaluates this.** The control plane, at issue, from its placement and lease records and the
identity of the node session the request arrived on. The agent only says which socket a request came
on, and the socket is the workload's because the agent made it for that container alone.

**What it does not stop.** A compromised node obtains credentials for every workload assigned to it,
no more than it has already (it runs them and reads their memory), and none for a workload placed
elsewhere. A credential stolen out of a workload works from anywhere for up to fifteen minutes. A
secret written into the workload's configuration would be worse on every count: stored in the clear,
re-delivered on every placement, readable with `workload.secret.read`, and surviving a move to a
node that should no longer hold it.
