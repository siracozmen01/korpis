# Capability tokens

A grant and a capability token are one object in two representations: the grant is the stored
form, the token the signed, self-contained wire form, with identical attenuation and evaluation.
This page covers tokens, share links, use limits, revocation lists, and what a host can see of a
tenant. Grants are on [grants](grants.md).

## Contents

- [The token](#the-token)
- [Offline verification](#offline-verification)
- [Keys and restore](#keys-and-restore)
- [Share links](#share-links)
- [Use limits are redeemed online](#use-limits-are-redeemed-online)
- [The revocation list](#the-revocation-list)
- [What a host can see](#what-a-host-can-see)
- [Explanations are filtered by grant](#explanations-are-filtered-by-grant)

## The token

```
CapabilityToken
  grant_chain     the chain's grant identifiers, leaf first
  actions, scope  one concrete object; selector scopes are resolved before minting
  issued_at, expires_at
  epoch           the issuing control plane's key epoch
  source_cidr     the leaf's networks, for the relay to check the client against
  stream_range    the leaf's range of a stream, for the node reading it to hold the session to
```

The token is a Biscuit whose authority block is signed by the control plane's token key
(`korpis-core/src/authz/token.rs`). The gateway mints one only after evaluating the whole chain
online, every link's attenuation and delegation included, so a token is the result of an
authorization, not a claim to re-derive. A holder can attenuate it offline by appending a block,
and a block only adds checks (a shorter expiry, a narrower operation), never authority. A verifier
needs only the chain's identifiers, so the revocation list can refuse a token when any link is
revoked; delegation policy constrains who creates grants, which happens online.

Two conditions travel with a token because their verifier is offline. The leaf's networks, because
the relay is the first hop that sees the client's address; the leaf's suffice, since attenuation
keeps a child's networks within its parent's. And a private action is judged offline against the
resource's ancestry, which a relay does not hold, so a token scoped to the resource itself verifies
(the gateway judged that very object) and one scoped above it is refused. The gateway always mints
for the one object the request names.

## Offline verification

Relays and agents hold the control plane's public keys and verify tokens offline, which is what
makes interactive work possible without a round trip per keystroke:

```
client attaches to a console
  -> gateway evaluates the chain online and mints {console.write on W, 120 s}
     with the route to W's node, sealed so the client cannot read it
  -> a relay verifies the token offline and opens the route
  -> the agent verifies it again, offline, before attaching
  -> keystrokes flow client -> relay -> agent, never through the control plane
  -> the session renews in band before expiry, each renewal minted by the gateway
```

A renewal must be the same subject's and is verified as the first token was. A session whose token
expires unrenewed is ended as `expired`; one whose renewal is refused, as `refused`. While the
control plane is reachable, revoking a grant ends every live session whose chain names it within
about two seconds, through the revocation list. While it is unreachable, revocation latency is the
token lifetime, which is why the lifetime is short and configurable rather than convenient, and a
console survives a control plane outage for exactly one token lifetime, then stops.

**Break-glass access** is the same mechanism: a long-lived token an operator pre-issues and holds
offline, which agents accept when the control plane is unreachable. Its use is recorded locally and
replayed to the effect log on reconnect, it is revocable through the revocation list, and its
existence is visible in the interface rather than a secret someone forgets. It never carries a use
limit, because a credential for outages cannot depend on a counter that needs the control plane.
Introducing it raises the revocation list's horizon in the same change.

## Keys and restore

Offline verification means a token's validity does not depend on the database that recorded it.
Restore an older backup, and a grant issued and revoked after it is gone from the database while
its token, signed by a key restored with it, still verifies. So rotating the token key is a
mandatory step of restoring the control plane, beside the lease epoch fence: every token issued
after the backup stops verifying, and every session re-authenticates, because a control plane that
cannot account for a grant must not honor it. Short token lifetimes are structural for the same
reason: the number that bounds revocation latency bounds how long a restore disrupts.

| Key | Signs | Rotated on restore |
|---|---|---|
| the token key | capability tokens, links and revocation lists, verified offline | yes, mandatorily |
| the node identity certificate | each node's key, pinned at enrollment | no |

They are separate because node identity and delegated authority have different lifetimes and
revocation stories. Rotating both would end a recovery with an operator re-enrolling every node by
hand during the incident.

## Share links

```
https://korpis.example/g/#korpislk_...
```

The fragment is the credential; holding the URL is holding the authority.

```
Moderator holds  workload.restart on project Survival, 90 days
attenuates to    workload.restart on workload W, 24 hours, 3 uses
sent in a Discord message: no account, no invitation, no provisioning
```

The link cannot outlive the moderator's grant, and revoking theirs kills it. Every use is recorded
with the full chain, so "who restarted it" resolves to this link, from this moderator, issued then.

It is a bearer token, and that is said out loud. The mitigations are all partial:

- The token is in the fragment, which browsers never send to servers and which appears in no access
  log or `Referer` header.
- Expiry and a use cap are mandatory: `korpis grant share` defaults to 24 hours and three uses, and
  the server refuses a link without both. The share screen shows both before the link exists.
- A link is for one object, never a selector, so its recipient sees exactly what it reaches.
- Irreversible actions are excluded by the same default as chat roles, and a link never delegates.
- A link is never placed where more than one person can read it. A surface offering "open this in
  the panel" from a channel or ticket offers a button that mints a fresh link for whoever presses
  it, under their own authority.
- It is not bound to the address of first use: a moderator on a phone behind CGNAT changes address
  within minutes, and a control that breaks the intended user teaches people to turn controls off.
  The use cap is what bounds a leaked link, by a number the issuer chose. Source ranges remain for a
  link issued to a known fixed network.

**A link is accepted by the gateway alone.** Every use begins at the gateway anyway (an action is
an API call, and a session needs the route only the gateway seals), and a relay accepting a link
would accept a revoked one for as long as the revocation list keeps it. So a link is a Biscuit
naming only its grant and expiry, with no right or scope, which the offline check of relays and
agents refuses for every action. The gateway checks its signature against the current key, its
expiry, and then the grant as it stands, through the same evaluation as any grant. Revoking a link
takes effect at its next call, and a session it opened ends within about two seconds. Its holder is
the subject `link:<grant>`, and can read the link's own grant (including who issued it), not the
chain above it. A replayed issue answers without the link.

**What counts as a use** is a redemption: the gateway authorizing a call, minting a session token,
or approving a plan. Renewing a session token spends nothing (the request names the token it
renews, and the gateway checks it is the same holder, chain and object, with no new action), so a
three-use link opens three consoles of any length. A watch spends one use when it opens and none
for the reads that keep it current. A proposer confirming its own plan continues the call that
proposed it, so propose-then-confirm is one use. Any check that spends nothing permits nothing to a
use-limited grant, so no path uses a link without counting.

**What it costs.** A link is unusable while the control plane is unreachable. Rotating the token
key ends every link, so a control plane restore ends links from before the backup too. A holder
can append a block that shortens a link before passing it on, and nothing else.

## Use limits are redeemed online

An offline verifier can check a signature and an expiry. A use counter is shared mutable state that
needs a central point of redemption, so it is checked only at the gateway, which states the trade:

| Condition | Checked where | Survives a control plane outage |
|---|---|---|
| signature, and the chain against the revocation list | relay and agent, offline | yes, as of the last list |
| expiry, not-before | relay and agent, offline | yes |
| source ranges | gateway for API calls, relay for sessions | yes |
| MFA, approval | gateway, online | no |
| use limit | gateway, online, redeemed atomically | no |

The feature that needs a counter gives up offline operation, rather than every token giving up a
guarantee. A use is spent when the call is authorized, not when it succeeds: the gateway takes it
by compare-and-swap on the grant row the moment the grant permits the call, and the call proceeds
only if the swap won. A call that then fails has spent its use. Spending in the transaction of the
change would make every use case responsible for remembering, and one that forgot would display a
limit nothing enforced. So the counter can be wasted, never exceeded, and a use case that must not
waste one (enrollment) checks what it can before it asks. Issuing a child from a parent's remaining
uses takes the same compare-and-swap in the transaction that inserts the child, so two concurrent
attenuations cannot both take the last three uses; the loser gets `CONFLICT`.

## The revocation list

Rotating the key to kill one credential would log out every console in the fleet. So relays and
agents hold a signed list of revoked grant identifiers, and refuse a token if any link of its chain
is on it. Listing grants rather than tokens makes the cascade work offline with no walk of the
subtree.

The list is signed by the token key, over a canonical form bound to its purpose
(`korpis.revocation.v1`, its version, then each identifier in order), so nothing else the key signs
reads as a list. An entry is a grant revoked, or suspended in its own row, within the horizon: the
longest lifetime of any token a verifier accepts (the 120-second session token) plus the clock skew
an agent session allows, since a verifier whose clock runs behind still honors a token the control
plane considers expired. Short-lived tokens need no entry of their own; their ancestors' cover
them. `LONGEST_TOKEN_LIFETIME` is pinned by a test, so a longer-lived token kind cannot arrive
without raising the horizon.

Every replica computes the list from the same rows every two seconds and publishes it to its
sessions when it changed; a session receives the current list on opening. No replica tells another
anything, so a revocation reaches a connected verifier within about two seconds through whichever
replica it is connected to. Each list replaces the last whole. The agent believes a list only once
its signature verifies, keeps it with its signature in its state directory (`revoked.binpb`), and
verifies the kept copy again at start, so a restarted agent refuses the same chains before it
reaches the control plane. It accepts a list with a lower version than its own, since replicas
compute independently and refusing a lagging one's list would hold a missing revocation only until
the next computation.

Derived suspensions (a suspended organization, an unverified binding) are not on the list, so their
tokens are refused at renewal, within one token lifetime, rather than within seconds. Putting them
on it would mean expanding each suspension into its grants every two seconds (the subtree walk
revocation avoids) or giving every verifier the ancestry of every resource, which no relay holds.
Suspension is a reversible commercial lever; a credential that must stop at once is revoked. So a
console or file session open when its tenant is suspended goes on for at most one token lifetime.

## What a host can see

Hosts are parents of their customers' organizations. Physically a host can always read what is on
its hardware; the question is whether the platform makes that invisible.

| Data | A parent organization's default access |
|---|---|
| resource usage, quota consumption, metering | yes: they pay for it |
| workloads, names, state, health, placement | yes: they operate the capacity |
| console content, file contents, secrets | no: an explicit grant naming the tenant |
| a backup's file tree | no: needs `backup.browse`, and browsing does not imply reading |
| a backup's contents | no: an explicit grant naming the tenant, and under tenant key custody not even then |

Operator access to tenant data is itself a grant, and when it is used the effect is visible to the
tenant whose data was accessed, not only in the operator's audit. "Your host can read everything
and you will never know" becomes "your host can read this, and you will see when they do."

## Explanations are filtered by grant

A placement's explanation names nodes, other nodes' capacity and why candidates were rejected.
Shown to a customer it would leak the provider's infrastructure and, through utilization, other
tenants. So explanations are stored complete and filtered per viewer on read; there are never two
stored versions of the truth.

| What | Shown to |
|---|---|
| a node's rejections and scores, and whether it was chosen | a viewer with `node.read` on that node |
| how many candidates, and how many failed each check | a viewer with `workload.read` over the workload's organization |
| which checks failed, without counts | anyone who may read the workload |
| the region (the chosen node's `region` label) and tier | anyone who may read the workload |
| the placement policies applied | a viewer who may read every node it names |

A placement's reason is written for every reader, so it names no node and no count. The same
filter applies to an unplaced workload's explanation, which is what a customer asking "why is my
server not starting" sees: the checks that failed, never whose nodes failed them. The same rule
holds wherever an object aggregates across a tenancy boundary: node status, cluster capacity,
scheduler recommendations.
