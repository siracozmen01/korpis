# Control plane restore

The store is the only stateful part of the control plane, so backing Korpis up is backing up one
PostgreSQL database, with continuous WAL archiving and point-in-time recovery. Restoring it while
agents run is dangerous, and this page is how it is made safe. The operator's procedure is in
[recovery](../user/recovery.md).

## Contents

- [The danger is authority](#the-danger-is-authority)
- [The epoch fence](#the-epoch-fence)
- [Fail static](#fail-static)
- [Recovering what nodes are running](#recovering-what-nodes-are-running)
- [Lost effects](#lost-effects)
- [Rotating the token key](#rotating-the-token-key)
- [What cannot be recovered](#what-cannot-be-recovered)

## The danger is authority

A restored store holds lease epochs from the moment of the backup, while agents in the field hold
newer ones. Under the fencing rule an agent presenting an older epoch is fenced, so the restored
control plane would consider every running agent stale while those agents consider themselves
authoritative: the split brain leases exist to prevent, reintroduced by the recovery. And a store
restored to Tuesday has lost what was declared on Wednesday, which nodes are running on Thursday,
and the grants revoked in between, whose tokens a restored key still verifies. So restore performs
three things before the restored control plane serves a request, none of them a procedure someone
can forget: it fences epochs, recovers declarations from the nodes, and rotates the token key.

## The epoch fence

A watermark of the highest epoch issued is in the database being restored, so it holds the
backup's value; every epoch issued afterwards is larger. Advancing past it alone clears nothing.
Two mechanisms work, and restore uses both:

1. **A bounded issue rate makes the watermark usable.** Epochs are issued from a bucket that holds a
   second's worth and refills at a recorded rate, so in any `t` seconds at most `rate x (t + 1)` are
   issued, and the fence is `W_backup + rate x (age + 1)`, the age rounded up to the second. Each
   node spends from a bucket of its own first (fifty a second, a hundred at once), so no node,
   releasing and asking again, takes the epochs every other node needs. Renewals spend nothing.
2. **Reconnecting agents raise the floor.** After fencing, the control plane opens a settling window
   in which a lease request that would issue an epoch is refused as unavailable, to be asked again.
   Each agent's welcome asks it to present every assignment it holds as signed; the highest lease
   epoch among those whose signature verifies, issued to the node presenting them, raises the floor,
   recorded as an effect naming the node. This closes the case where the first bound was computed
   from a wrong backup age.

A broken or hostile agent could present an enormous epoch and exhaust the space, so epochs are
believed only inside assignments the control plane signed, against every assignment key that ever
signed (a key's end bounds what it signs from then on, not what it signed before). An epoch is
authority, and no unverified claim of authority is taken from anywhere. Presentations are believed
only while a window is open. What the second mechanism does not see, a lease epoch newer than the
node's last signed assignment, the first bound covers. Alone, the first is safe but conservative and
the second exact but dependent on agents appearing; together the fence is correct whether every
agent reconnects or none does. `korpis-server restore` fences from the backup's watermark and rate
and leaves the window for the next start to open.

## Fail static

Agents that reconnect and find their epochs stale freeze mutations and leave processes running, then
ask for reissue by their expiry policy. Fencing exists to stop two authorities mutating one
workload; a fenced agent already refuses mutations, so killing what it supervises prevents nothing
more and turns a control-plane incident into a tenant outage. So `keep_running` is the derived
default for every workload whose storage no other node can attach or start it from, and `stop` stays
the default where correctness depends on single ownership of a shared resource or of the data a
replica copies.

## Recovering what nodes are running

The fences protect authority, not configuration. Restored to Tuesday while Thursday's agents run
Wednesday's intents, the store's head for a workload is version 5 while its node runs 7: a resync
would roll it back silently, with no plan and no record; the next declaration would be numbered 6, a
version that existed with another body; and a workload created Wednesday has no row, its runtime
objects orphaned.

Every assignment is signed by the control plane that issued it and carries the target intent, the
workload's owner, name and labels, its power, its whole node plan, its active runs and its lease
epoch. During the settling window, for every presented assignment whose signature verifies, in a
transaction of its own and with an effect naming the node, the key and the signature:

| The restored store lacks | It records |
|---|---|
| the workload | a recovered workload under its signed owner and name (or the name plus the first eight hex digits of its id, when a workload the store holds took the name) |
| the target intent | a recovered intent with its identifier, version, body and declarer, `recovered_from` set; a parent or declared set the store lacks is left out |
| the power declaration | the recovered power declaration |
| newer head, target or power pointers | each moved forward only if the store's is older, with the version floor past both, so no version is issued twice |
| the placement | the workload bound to the presenting node; moved from another node only if the presented epoch beats both the store's and the highest presented for that node, and every other placement removed |
| the node's id slot for it | given to it, unless the store gives it to a workload placed on that node, which `restore.id_slot.conflict` reports rather than give one range of host ids to two tenants |
| the plan the node is executing | recovered as signed (kind, every step, approval and approver, its grant if held) as applying, marked recovered; any other applying plan of the workload is cancelled first |
| active runs | recovered as running, under the intent they name or the target, with the schedule moved past them |

Runtime objects are labelled with intent identifiers, never versions, so even a version that slipped
through could not make a new declaration look applied. Nothing is recovered from an assignment that
does not verify, or that is older than what the store holds (a plan computed before one that has
started, a run for a cycle already made); `restore.recovery.refused`, `restore.plan.refused` and
`restore.run.refused` say why. A second presentation changes nothing. Recovery is not admission: a
recovered workload counts against quota and the node from then on, even past its limits, since it is
already running. Nothing goes down an agent's session while the window is open, and the run maker
makes and ends nothing, since the store's runs and leases are the backup's until nodes present.

**A workload retired after the backup** is live and placed in the restored store, and its node
presents nothing for it, or only the residual assignment a node keeps of what it retired. So every
placement on a presenting node whose workload it did not present is held, left out of what the node
is sent, until an operator confirms it (`ConfirmPlacement`, with a reason) or retires the workload,
or a later presentation includes it. The hold cannot tell a retirement after the backup from an
assignment made just before it and never delivered, which is why it waits for an operator. A node
that does not reconnect during the window presents nothing, holds nothing, and is sent what the
store says when it returns.

**What recovery does not restore**: a recovered workload's membership in a declared set, operator
classification overrides and the unreproducible mark, none of which an assignment carries; a run
that ended after the backup, which is in no assignment; and an occurrence that came due after the
backup and finished, which the restored store sees as due, so a schedule with catch-up runs it
again. A job that must not run twice makes that safe itself, as a retry after a lost node already
requires.

## Lost effects

Agents keep acknowledged journal records for a retention window (seven days by default). The
restored cursor is the backup's, so a reconnecting agent resends everything after it, and the effect
log refills itself exactly once, in order. A backup older than the window leaves a gap, and the
agent answers a cursor older than its journal's tail with a gap record for the numbers it no longer
holds. The restore writes `restore.gap` covering the backup's time through the restore's, naming the
operator, for control-plane effects with no node counterpart.

## Rotating the token key

Agents verify capability tokens offline against a signing key. Restored to Tuesday, a grant issued
Wednesday and revoked Thursday is gone from the database, while its token, signed by a key restored
with it, still verifies: revoked authority resurrected with no row recording it. A legitimate grant
from Wednesday keeps working while the interface shows nothing, so nobody can revoke it. A restored
control plane must invalidate authority it can no longer account for.

The token key has epochs: the data directory holds `token-<n>.key`, and the control plane signs with
the highest. Rotating makes the next, records its public half, retires every earlier key in the
store, and ends every session in the same transaction, recorded as an effect naming the operator.
Every agent session opens with the token keys in force, before anything signed with them; the agent
replaces its set with them, keeps them for its next start, and verifies tokens and revocation lists
with them, and relays learn the same set from their feed. An agent accepts a retired key's tokens
until it reconnects, the same window a control plane that is down leaves anyway. A rotation that
fails part way must not leave both trusts standing: the new key is written under a name no load
reads, recorded in the transaction retiring the rest, and put in place only after that commits; its
epoch passes every epoch on disk or in the store, and a name recorded with other bytes is refused.
`serve` refuses to start when the store trusts a token key other than the one its data directory
signs with. The assignment key is never rotated, since nodes present what it signed, and node
identity certificates are never rotated by restore, which would mean re-enrolling every node during
the incident.

The cost is a bounded, visible interruption during a disaster the operator is already handling; the
alternative is authority in circulation with no record of it. Short token lifetimes bound how long
the disruption lasts.

## What cannot be recovered

A declaration made after the backup and never approved reached no node, so no node can present it;
it is lost. Control-plane effects with no node counterpart between the backup and the failure (an
approval, a grant issued and never used) are lost, and the gap record says so. Grants issued in that
interval are void, since the key rotated. The interval is as long as the backup is old, which is why
continuous WAL archiving, not nightly dumps, is the documented configuration.
