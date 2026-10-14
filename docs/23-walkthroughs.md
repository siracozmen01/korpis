# Walkthroughs

**Status:** design **Date:** 2026-08-07 **Depends on:** every preceding document

---

## 0. What this document is

Twenty-three documents, each internally consistent. That is not the same as working together.

Designs do not fail inside a document. They fail **between** documents, where one says "this is
handled in the storage layer" and the storage layer never heard about it. Per-document review
cannot find those, because each document reads correctly on its own.

So: fourteen concrete scenarios, traced end to end through every layer they touch. The output is
not a tutorial. It is a **defect list**, and it is written to be adversarial, the goal of each
trace is to find the step where nobody is holding the ball.

**Fourteen traces produced twenty-three findings, and all of them are now closed** and patched into
the documents they belong to, the table in §15 says where each landed. Each cost a paragraph to fix
here. Finding them in month nine would have cost a rewrite.

§16 records a separate external review that attacked the documents' internal algebra rather than
the seams between them, and found seventeen more. §17 records a second external review, in October
2026, that re-read every document against every other and against the primary sources, and whose
seventy-nine findings changed the mechanics several of these traces cross.

§18 is a fifteenth trace, written from the implementation in October 2026 when deletion was
built, and its finding is numbered on from the first fourteen. Finding 25 came from re-running
§4 when recipe trust was built, and Finding 26 from extending §6 when the agent's journal was
built; each is numbered on from the one before. §19 is a sixteenth trace, written when snapshots
were built, and Findings 28 to 31 came from the implementation of streams and volumes. §20 is a
seventeenth, written when backups were built, and it found Findings 32 and 33; §21, the
eighteenth, written when restores were, found 34 and 35.

**The traces were re-run on 2026-10-12 against those changed mechanics.** Where a step now runs
through a different mechanism, its row says so and cites the section that owns it; the findings and
their resolutions are left exactly as they were found, because they are the record. A row marked
† changed in the re-run.

The first seven follow one workload through its life: created, migrated, orphaned by a dead node,
delegated, attacked, restored, upgraded. That is one axis, and after they were written the coverage
was measurably lopsided: `05-scheduling.md` was crossed seventeen times and `13-surface-cli.md` not
once. The next seven were chosen by that measurement rather than by intuition, and they follow the
things that surround a workload instead: the meter, the backup, the pipeline, the identity
provider, the package, the edge, and the extension on the critical path. They produced more
findings than the first seven did, which is what an untraced surface is supposed to do.

---

## 1. Creating a workload, and sharing it

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | Subject declares a workload; grant checked at declaration | `08-identity.md` §3 |
| 2 | Recipe reference `…:1.20` resolved to a digest, decided under the organization's trust policy before anything is fetched, and **recorded in the intent** with what vouched for it; what the declaration leaves out is completed from that recipe, once ‡ | `09-recipes.md` §8, §7.1, §3.1 |
| 3 | Config validated against the recipe's schema, per-field permissions applied | `09-recipes.md` §5 |
| 4 | Quota checked | `05-scheduling.md` §5 |
| 5 | Placement: static filter, score, a `FeasibilityQuery` to the shortlist over their streams, bind in the approving transaction under the placement lock; `Explanation` recorded, or the workload recorded as unplaced with it † ‡ | `05-scheduling.md` §2.1 to §2.3; `02-architecture.md` §4.3 |
| 6 | Plan persisted; non-disruptive by the drivers that could run it and by the recipe's declared `disruption`, so auto-approved † | `01-model.md` §3.3; `04-runtimes.md` §4.1; `09-recipes.md` §5.1 |
| 7 | `Intent` v1 written; approval moves `target` by compare-and-swap; `Effect` in the same transaction † | `03-state.md` §3.1, §3.2, §4 |
| 8 | Agent receives the workload's signed `Assignment`: target intent, power, plan steps, and the recipe document; the plan becomes `applying` as it is sealed † ‡ | `02-architecture.md` §4.8 |
| 9 | `Prepare` from the intent and the carried recipe alone, configuration written as variables and arguments; content fetched by digest, signature verified, **pure** ‡ | `04-runtimes.md` §4.2; `09-recipes.md` §5.2 |
| 10 | Volume created by the helper with byte and inode quota in the write path, or, in a `directory` class, with none, declared as none ‡ | `06-storage.md` §2, §3; `02-architecture.md` §6.2 |
| 11 | Endpoint allocated, exposure mode applied | `07-networking.md` §3 |
| 12 | `Create` over the driver's socket, to a peer the kernel's credentials name, the spec validated again; then `Start`, which the driver reports as an event ‡ | `04-runtimes.md` §4.2; `02-architecture.md` §6.1 |
| 13 | Health probed at the application level | `04-runtimes.md` §5 |
| 14 | `Observation` reported; node-originated effects journaled and acknowledged exactly once † | `02-architecture.md` §4.3, §4.10 |
| 15 | Web shows converging, then converged, with the lag, each change pushed to its watch as it commits ‡ | `11-surface-web.md` §3; `10-api.md` §7 |
| 16 | Owner issues a share grant; token travels in the fragment; any `max_uses` is redeemed online † | `08-identity.md` §6.1, §6.2 |

‡ Updated 2026-10-13, from the implementation. The trace had the subject declare a complete
body; the declared file of §3 of `13-surface-cli.md` is not one, and nothing said who completes
it. The control plane does, at step 2, against the digest it just resolved, so step 3 validates
the configuration of the body that will run and step 6 classifies its real steps. Step 5 had no
answer for a workload no node can take, and none for two creates racing for the last free
capacity: the bind now happens in the transaction that approves the target, under one placement
lock, and a target with no node is recorded with its explanation, shown as unplaced, and retried.
Placing the example recipe also showed that §9 of `07-networking.md` kept any workload with an
`overlay_only` endpoint off a node without the overlay; it is corrected there. Step 15 had a
stream with no source: nothing said what wakes a watch, and a watch that polls is either late or
expensive. The database announces each committed change, and the watch re-reads on it. Step 8
had no rule for when a generation moves or when an approved plan becomes applying, and an
assignment signed over bytes that encode differently each time could never be recognized as
unchanged: the schema's maps are now ordered, an unchanged assignment keeps its generation, and
the plan becomes applying in the transaction that first seals it. Step 2
named no trust decision, though §7 of `09-recipes.md` makes one: the control plane decides it
there, on the digest and the signatures kept with the recipe, under the policy that governs the
declaring organization, and a dry run shows a refusal before anything is written. Step 9 had no
rule for how a configured value reaches the software: an `arg` field's template was never
defined, and the example schema of §5 of `09-recipes.md` could not be loaded. §5.2 there now says
how each value is written, and §4.2 of `04-runtimes.md` what the driver reads and what it refuses.
Step 12 crossed a socket nothing guarded: §6.1 of `02-architecture.md` made the driver validate
what it is asked, but not say who may ask, and a driver that holds containerd's privilege must.
The kernel's record of the connecting user now decides it, and `Wait`, which the protocol never
carried, is its `Events` stream. Step 10 assumed every node could enforce a volume's size; a
filesystem without project quota cannot, and no class described one. The `directory` class of §2
of `06-storage.md` does, with every capability false, and the helper that creates the volume is
reached by a transport §6.2 of `02-architecture.md` now defines. On ext4 and XFS the limit is a
quota project per volume, read from the filesystem and checked in a test against a real one (§3
of `06-storage.md`), and the section now says what passes it: root on the host. The first run of
the whole trace on a real node (`crates/korpis-e2e`) stopped at step 5 twice. The node reported no
disk, because §2.1 of `05-scheduling.md` deferred disk to Phase 1 while the scheduler already
counted volumes; and a preset with no console asked for `logs`, which no Phase 0 driver offers.
Disk is reported from the helper's classes, and §3.1 of `09-recipes.md` now says what the console
default costs. Step 1 through the CLI showed one more gap: `korpis plan` was to return a plan that
`apply --plan` could name, and the API's only way to compute a plan without applying it, `dry_run`,
persisted nothing. `propose` persists the plan unapproved (§4 of `13-surface-cli.md`, §3.3 of
`01-model.md`). Running the trace once more as an operator would, with the five binaries as
processes and only the CLI and `oras` (`scripts/e2e-phase0.sh`), found the last two: the done-when
of §4 of `20-roadmap.md` asked a denial to name a grant, which nothing authorizing it can supply,
and the scheduler's bind and the lease it issued named no grant though both carry out an approval.
The done-when is corrected in place, and both effects now name the grant of the decision they
carry out. The same run's refused stop is a single denial; a client retrying it in a loop would
have written one effect per attempt, the unbounded write path finding S15 closed on paper only.
Repeats are now coalesced as §10 of `10-api.md` says, with the count held in the database so that
a restart neither loses nor doubles it, and reading the log showed that an effect's `error`,
which names the missing action, had never reached the wire; it does now. Step 6 named a
driver layer nothing could fill: no driver could say that a step costs more on its tier, so
every plan classified that row by the vocabulary alone. Drivers now declare it per tier (§4.1 of
`04-runtimes.md`), and the control plane reads it from the nodes that could run the workload.
The operator's row, the last one, had no field and no action either; it is set with
`workload.classify` (§5 of `10-api.md`), on the record, and can never undercut the floor.
Step 5 placed the game endpoint by its declared `stable`, so a node behind NAT could take it on
an installation with no edge tier to hold the address; the scheduler now judges reachability on
the mode the declaration resolves to (§3 of `07-networking.md`). The step's `FeasibilityQuery`
is the one part of it Phase 0 does not run: the shortlist query and the re-placement after a
post-bind rejection are homed in Phase 1, with the question of the placement lock they raise
(§2.1 and question 9 of §10 of `05-scheduling.md`).

**FINDING 1: Quota is checked, but not held. `CONFIRMED`**

Step 4 checks quota; step 7 writes the intent. `10-api.md` §6 protects the *workload* with
compare-and-swap on `expected_version`, and that is the wrong object: quota is a property of the
organization, not of the workload being created. Two concurrent creates in the same organization
each check 24 Gi of 32 Gi used, each pass, and both commit, 8 Gi over.

This is not exotic. It is a customer clicking twice, a Discord command racing a web action, or a
pipeline creating a batch.

**Resolution.** Quota consumption is written in the same transaction as the intent, against a row
whose version participates in the CAS, the quota entry, not the workload, is the contended object.
A quota check that does not take a lock on what it is checking is a race, and the honest name for
the current design is "advisory quota", which P4 forbids displaying. → patch `05-scheduling.md`
§5.1 and `03-state.md` §3.2

**FINDING 2: A Plan's steps have no stated atomicity. `CONFIRMED`**

Steps 10–12 are three durable side effects. If the volume is created and endpoint allocation fails,
what exists? `05-scheduling.md` §3 gives `Operation` ordered steps across *many* workloads, and P9
promises every operation is resumable and reversible, but nothing states the semantics for the
steps *within* a single workload's plan.

**Resolution.** A Plan's steps are ordered, individually idempotent, and resumable from the last
completed step; a failed plan leaves a workload in a state the model can represent, which is what
P9's "no operation may leave a workload in a state the model cannot represent" already demands and
never operationalizes. Partial resources are retained and named in the observation, never orphaned
silently. This is the same treatment `02-architecture.md` §4.4 gives adopted runtime objects. →
patch `05-scheduling.md` §3

*Confirmed correct in this trace:* recipe digest resolved **once, centrally, at step 2** rather
than per node, two agents cannot resolve the same tag differently. This was the seam I expected to
find and `09-recipes.md` §8 already closes it.

---

## 2. Migrating a busy server

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | Operator drains a node; `Operation` with `max_unavailable` | `05-scheduling.md` §3 |
| 2 | Per workload: destination chosen, `Explanation` recorded | `05-scheduling.md` §2 |
| 3 | Volume snapshot; incremental transfer while running | `06-storage.md` §8 |
| 4 | Final incremental with the workload paused | `05-scheduling.md` §8 |
| 5 | **Cutover:** the migrate plan's step in both nodes' assignments; source lease released, destination started † | `02-architecture.md` §4.5, §4.8; `05-scheduling.md` §8 |
| 6 | Edge forwarding target updated in the same step; the residual window is stated, not denied † | `07-networking.md` §3.1, §10 |
| 7 | Meter cut at that instant, no reset, no double count | `15-observability.md` §2 |
| 8 | Players reconnect to the same address | none |

**FINDING 3: The console does not migrate. `CONFIRMED`**

`14-streams.md` §4 puts segment files in the tenant's storage on the node, and the agent owns all
three tiers. Nothing in the migration sequence moves them. So a workload migrates and its
scrollback stays behind on a machine that is about to be decommissioned, and §1 of that same
document argues at length that the console is *evidence*, not a widget.

Worse: offsets are per-stream and monotonic. If the stream restarts on the destination, either
offsets restart (breaking every reader's resume) or the destination must know where the source
stopped.

**Resolution.** The stream is part of the workload's durable state and migrates with the volume, in
the same incremental transfer. The destination continues the offset sequence, which it can only do
if the final offset crosses in the cutover message, so the offset watermark joins the lease epoch
as something the cutover carries. Any range that could not be transferred is a **gap record**, so a
migrated console is honest about what it lost. → patch `14-streams.md` §4 and `05-scheduling.md` §8

**FINDING 4: A client attached to the console during cutover. `PLAUSIBLE`**

The reader is connected to the source node with a scoped token. At cutover the source is fenced.
The reader sees the connection drop.

This is probably correct behaviour and does not need machinery, the reader reconnects, is directed
to the destination, and resumes at its last offset, which Finding 3's resolution makes possible. It
is recorded because it is only correct *given* Finding 3; without it, the reader resumes at an
offset that means something different. → verify after patching

---

## 3. A node dies

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | Node heartbeat stops; after three declared intervals its `connectivity` becomes `unreachable`, its `admin` untouched † ‡ | `02-architecture.md` §9, §4.2; `01-model.md` §3.4 |
| 2 | Observations become `unknown`, never a stale value shown as current | `11-surface-web.md` §2 |
| 3 | The agent, unable to renew, fails static; the grantor's lease expires on its own clock; the workload's derived `on_lease_expiry` decides † | `02-architecture.md` §4.5 |
| 4 | Metering records a gap with its cause | `15-observability.md` §2 |
| 5 | Workloads on replicated storage whose class fences the old writer are rescheduled † | `18-operations.md` §5; `06-storage.md` §2 |
| 6 | Workloads whose only copy of data was local are **not**, and this is said plainly | `18-operations.md` §5 |
| 7 | Its consoles survive as far as offload reached: the control plane's segment index finds the offloaded segments, and the offsets held only on the node become a gap ¶ | `14-streams.md` §3, §4; `01-model.md` §3.6 |
| 8 | On return: a fresh signed Hello replaces any half-open session; adoption by label, epoch checked; if fenced, it cannot resume; a node cordoned before it dropped out returns cordoned ‡ | `02-architecture.md` §4.4, §7; `01-model.md` §3.4 |

‡ Updated 2026-10-13, from the implementation. Node status was one field, and the lifecycle took
an unreachable node back to `ready` on reconnect, so a node cordoned for maintenance that lost its
network for a minute came back taking placements. What the operator decided and whether the node
is heard are now two states, and the status a client shows is derived from both. Building the
session showed that step 1 had a declared heartbeat interval nothing told the agent, and no rule
for a stream that stays open while its agent is gone: the first answer to a Hello now declares the
interval, and three missed heartbeats make the node unreachable whether or not its stream closed.
A node cordoned for maintenance can still be given one workload deliberately: `PlaceWorkload` binds
it there by hand, overriding the cordon and taints and nothing else (§2.3 of `05-scheduling.md`),
which is what `bound_by: operator` had promised without anything to do it.

¶ Added 2026-10-13, from auditing `01-model.md` against the wire schema. The model gave a stream
its `segments` and the schema had no field for them, and nothing said who held that list: the
agent owns every byte of a stream, so after this step's failure the one component that knew which
segments had been offloaded was the one that died. The agent now journals each seal and offload,
and the control plane keeps the index from those records (§4 of `14-streams.md`).

**FINDING 5: What a `stable` address does while its workload is gone. `CONFIRMED`**

The edge owns the address and forwards to a placement that no longer exists. Nothing says what it
does. The three possibilities are meaningfully different to the person connecting: a black hole
(they wait thirty seconds and blame their own connection), a refusal (fast, honest, generic), or a
protocol-aware status (accurate, only for protocols the router understands).

This is exactly the affordance `22-first-party.md` §6 already built for hibernation, a router that
answers a Minecraft ping with "starting, ~30s". An unreachable workload is the same shape of
problem with a different message.

**Resolution.** Default is immediate refusal, never a black hole, a timeout is P4's "unknown"
rendered as a stall. Where a protocol router is present, it answers with the real state:
`hibernated`, `starting`, `unreachable`, or `data unavailable`. → patch `07-networking.md` §3.1

*Confirmed correct:* step 6 is the honest one, and it is already honest. A design that implied
automatic recovery of a workload whose only data copy died would be the most damaging possible lie,
and `18-operations.md` §5 refuses to tell it.

---

## 4. A reseller's customer's customer

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | Operator delegates to a reseller: a grant, scoped to an organization | `08-identity.md` §3 |
| 2 | Reseller delegates to a customer, attenuated, never wider, as the grant's `delegation` policy allows † | `08-identity.md` §3.2 |
| 3 | Customer delegates to their own moderator | same mechanism, no new code |
| 4 | Quota inherits down the tree | `05-scheduling.md` §5.1 |
| 5 | Metering rolls up; the reseller sees their sub-tree's usage | `15-observability.md` §2 |
| 6 | `Explanation`s filtered per viewer, the customer learns nothing about the node | `08-identity.md` §8 |
| 7 | No reseller feature was written anywhere | Bet 4 |
| 8 | The customer enrolls a machine of their own: it belongs to their organization, only their subtree lands on it, and the reseller's grant covers it by containment ‡ | `01-model.md` §3.4; `08-identity.md` §3.3; `05-scheduling.md` §2.1 |
| 9 | The customer declares from a recipe: the reseller's trust policy governs until the customer sets its own, and a removal returns to the reseller's ‡ | `09-recipes.md` §7.1 |

‡ Added 2026-10-13, from the implementation. A node had no owner, so nothing said whose workloads
could land on a customer's own machine, and no grant above the node itself could read or cordon
it. Enrollment was already scoped to one organization; the node now belongs to it. Building
enrollment showed that step 8 rested on a `max_uses` nothing counted, and that the token's request
named no organization at all: the token now binds the organization, the name, and the labels, and
its one use is redeemed at authorization (§3 of `18-operations.md`, §6.2 of `08-identity.md`).
Building `korpis node token create` showed the example in §3 of `18-operations.md` passing the
node's name as a label; the token binds a name and labels separately, and the example now says
so. Step 9: the
trust policy was per organization and the customer had none, so no policy governed the customer's
declarations at all. The nearest ancestor's now does. The customer may set one the reseller would
not, because the policy protects the workloads that run the recipe, and the reseller's machines
are protected from those workloads by their isolation tier, not by a signature; whether a reseller
may still pin a floor is question 7 of §10 of `09-recipes.md`. Extensions differ, and FINDING 7
below is why: an extension acts with grants and egress, so the node owner's allow-list gates it.

**FINDING 25: An organization without its own trust policy had no policy at all. `CONFIRMED`**

§7 of `09-recipes.md` defined the policy per organization and said nothing of an organization that
has not set one, which is every organization an installation creates after its first. The
evaluator at step 9 had nothing to read: either every customer's declarations failed until someone
thought to set a policy for each, or each was silently given the default and its reseller's
decision never reached it. Neither is stated anywhere, so an implementation would have picked one.

**Resolution.** The nearest ancestor's policy governs an organization that has none, a removal
returns to it, and the version keeps counting through the removal so compare-and-swap still
holds. The cost, that a parent cannot impose its policy as a ceiling, is stated and routed. →
patched `09-recipes.md` §7.1, question 7 of §10

**FINDING 6: Quota inheritance does not say allocation or usage. `CONFIRMED`, and it decides Bet
4**

A reseller holds 32 Gi. They sell 16 Gi to customer A and 16 Gi to customer B, then want to sell 16
Gi to customer C, because in practice, customers use a fraction of what they buy, and overselling
is not an abuse of this market, it *is* this market.

If child quota consumes parent quota **by allocation**, the reseller is blocked at 32 Gi and Korpis
has forbidden the standard commercial practice of every host it is courting. If it consumes **by
usage**, then quota is not a guarantee and P4 is violated the first time three customers get busy
together.

Neither is acceptable, and the design currently states neither.

**Resolution.** Both, explicitly, per the same distinction `05-scheduling.md` §5.2 already draws
for resources: a quota entry has a **reservation** and a **limit**. Reservations sum against the
parent and cannot be oversubscribed; limits may exceed it, by a declared ratio, and the sum of
limits is visible to whoever set it. A reseller sells 16 Gi limits with 4 Gi reservations and knows
exactly what they have promised versus what they have guaranteed, which is what they are actually
doing today with a spreadsheet.

The overcommit ratio is per-dimension and memory defaults to off, exactly as §5.3 already requires.
Bet 4 survives, and it survives because the mechanism already existed one layer down; it had simply
never been applied to quota. → patch `05-scheduling.md` §5.1

**FINDING 7: Who approves an extension's grants inside a delegated organization. `OPEN`**

`16-extensions.md` §4 says an extension declares what it needs and "the operator" approves. In a
three-level tree there are three parties who could reasonably be called the operator, and an
extension runs as a workload on the *machine owner's* hardware with declared egress.

A reseller's customer installing an extension is asking to run code on someone else's machine. The
grant model already prevents authority escalation, the extension cannot exceed its installer. But
egress, resource consumption, and reputation are the machine owner's exposure, not the installer's.

**Decision needed.** The likely shape is that installation requires the installer's grants *and* an
allow-list controlled by whoever owns the node, defaulting to first-party extensions only. That is
a policy choice with real product consequences, not something to settle inside a walkthrough. →
`16-extensions.md`

**Closed.** §4.1 of `16-extensions.md` settled it in the shape proposed: installation needs the
installer's grants and the node owner's allow-list. The `OPEN` above is how the trace found it.

---

## 5. A hostile tenant

**Trace**: each attempt, and what stops it.

| Attempt | Stopped by | Governed by |
|---|---|---|
| `../../etc/shadow` through the file API | `openat2(RESOLVE_BENEATH)` + Landlock; the string is never a decision | `17-security.md` §4 |
| symlink race in an upload | same, the kernel resolves or it does not | `17-security.md` §4 |
| fork bomb | cgroup `pids.max`, no reservation, no way to disable | `05-scheduling.md` §5.2 |
| a million empty files | inode quota | `06-storage.md` §3 |
| 50 MB/s of log output | stream rate limit, drop with gap marker, never blocks the workload, counts against their own quota | `14-streams.md` §4, §5 |
| exhaust the node's conntrack table | per-workload conntrack limit | `07-networking.md` §6 |
| reach `169.254.169.254` | the egress default denies link-local metadata; the public internet is allowed, rate-limited † | `07-networking.md` §6 |
| reach a neighbour | network namespace + default-deny | `07-networking.md` §6 |
| read the daemon's credentials via a template | no ambient scope; a flat pre-resolved variable map | `09-recipes.md` §6 |
| escape the kernel via an LPE | **not stopped** in `process`/`container`; this is why the tier is a choice | `17-security.md` §5 |
| exfiltrate their own operator's secret from a log | redaction at write time, best-effort, labelled as such | `14-streams.md` §7 |

**FINDING 8: Egress policy is enforced by the party being contained. `OPEN`, already known**

`07-networking.md` §11.6 raises it and hands it to `17-security.md`, which does not answer it.
Egress rules are enforced at the source node by that node's agent. A compromised agent can ignore
them, and a compromised agent is explicitly in the adversary table (`17-security.md` §2).

This trace does not resolve it, it confirms the handoff is dangling, which is the same class of
defect as the edge-availability one already fixed. The options are real and costly: enforce at the
edge (latency, a chokepoint), enforce at the destination (only works inside the fleet), or accept
it and state that a compromised node's egress is uncontained. **Decision needed.** →
`17-security.md`

**Closed.** §9.1 of `17-security.md` decided it. The `OPEN` above is how the trace found it.

*Otherwise this trace is the strongest in the document.* Every attempt except the kernel LPE meets
a named, specific mechanism, and the LPE is not defended against by anyone and is labelled
honestly.

---

## 6. Restoring the control plane from backup

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | Postgres, signing keys, and the assignment-signing keyring restored † | `18-operations.md` §6 |
| 2 | **Epoch floor raised past every epoch that could have been issued**, after a settling window † | `03-state.md` §8 |
| 3 | Control plane starts; agents reconnect | `02-architecture.md` §4 |
| 4 | Each presents a digest of its assignments; assignments newer than the backup whose signatures verify are recovered, not rolled back † | `03-state.md` §8; `10-api.md` §9 |
| 5 | Workloads never stopped, they were never dependent on the control plane | `02-architecture.md` §4.6 |
| 6 | Each agent resends its journal from the restored cursor; numbers it already discarded are answered with one gap record, which the cursor passes ‡ | `02-architecture.md` §4.10; `03-state.md` §8 |
| 7 | Relays running as their own process reopen their feeds; nodes are named the relays again § | `02-architecture.md` §3.1; `18-operations.md` §6 |

‡ Added 2026-10-13, when the agent's journal was built; it found Finding 26.
§ Added 2026-10-14, when relays ran as their own process; it found Finding 27.

**FINDING 27: A relay removed after the backup returns with it. `CONFIRMED`**

A relay obtains its feed with its identity key, and removing it is the only revocation of that
key. Restore to a backup older than the removal and the relay's row says joined, its key still
verifies its feed request, and every node is named it again within seconds. A relay sees the
plaintext of the sessions it carries (§8 of `17-security.md`), and the usual reason to remove one
is that it can no longer be trusted, so this is Finding 9 again for a third kind of authority:
the restored control plane honours something it can no longer account for.

**Resolution.** Restore holds every relay the backup records as joined: its feed is answered as
unavailable and it is named to no node until an operator readmits or removes it. A relay that
joined after the backup is unknown and is refused, and joins again. Holding is chosen over
rotation for the reason node identities are not rotated (Finding 16): readmitting a relay is one
call, re-enrolling one is a visit to its machine during the incident. The cost is that sessions
open only through the control plane's own relay role until the relays are readmitted. → patched
`18-operations.md` §6

**FINDING 26: A restored cursor older than a node's journal waits forever. `CONFIRMED`**

§8 of `03-state.md` said a backup older than the retention window "leaves a gap, and the gap is
recorded as one", and nothing recorded it. The control plane admits only the record one past its
cursor (§4.10 of `02-architecture.md`); the node had discarded those records after their window,
so the first record it could send was past a hole nobody could fill. Every later record from that
node waited behind the hole, so a node whose backlog outlived the window lost not only the
discarded records but every record it would ever send. The claim named an outcome and no
component that evaluates it: the control plane cannot know what a node discarded, and the node did
not know it was meant to say.

**Resolution.** A gap record stands for every number it names, numbered by the last of them, and
the control plane admits it when the first is no more than one past the cursor. The agent, which
alone knows its tail, answers a cursor older than it with a gap record whose cause is
`discarded before the control plane ingested it`, made as the journal is sent; the same rule
carries `journal exhausted` and `journal damaged`. A record that cannot be read as one is kept as
a rejection with its number, so it cannot block a journal either. → patched `02-architecture.md`
§4.10, `03-state.md` §8

**FINDING 9: Capability tokens outlive the database they were issued from. `CONFIRMED`**

`08-identity.md` §6 makes the grant and the capability token the same object in two
representations, and the agent verifies tokens **offline** against a signing key. That is what
makes the system work during an outage, and it has a consequence nobody wrote down.

Restore to Tuesday's backup. A grant issued Wednesday and revoked Thursday is gone from the
database, but its token is signed by a key that was also restored, so it still verifies. **A
revoked grant is resurrected, silently, and there is no row anywhere saying it exists.** The audit
trail is not merely incomplete; it actively disagrees with reality.

The symmetric case is milder and also wrong: a legitimate grant issued Wednesday keeps working
while the panel shows nothing, so it cannot be revoked through the interface.

**Resolution.** Restore rotates the signing key, exactly as it advances the epoch watermark, and
for the identical reason: a restored control plane must invalidate authority it can no longer
account for. Every token issued after the backup stops verifying, and every session
re-authenticates. The cost is a visible, bounded interruption during a disaster the operator
already knows about; the alternative is silent authority with no record.

Key rotation therefore joins epoch advancement as an **unskippable restore step**, and the token
lifetimes of §6 of `08-identity.md` bound how long the disruption lasts. → patch `03-state.md` §8,
`08-identity.md` §6, `18-operations.md` §6

---

## 7. Upgrading a fleet

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | Control plane first; migrations forward-only and online | `18-operations.md` §4 |
| 2 | Agents upgraded one at a time, over days if wanted, within the window defined by supported releases † | `10-api.md` §4 |
| 3 | Each agent restarts and **re-adopts running workloads**, no tenant restart | `02-architecture.md` §4.4 |
| 4 | Extensions declaring an incompatible API version are refused at load, with a message | `16-extensions.md` §6 |
| 5 | Mixed-version fleet is a supported steady state, not a window | `18-operations.md` §4 |

**Nothing new found**, and one behaviour worth naming because it is easy to get wrong: an extension
refused at load in step 4 that happens to be a *provider* (a DNS provider, say), leaves its
dependents in `unsatisfiable` with the provider named, which is the same treatment
`16-extensions.md` §5 gives a provider that is merely down. A refused-at-load provider and a
crashed provider produce the same observable state, and that is correct: from a workload's
perspective they are the same event.

`18-operations.md` §10.5 already proposes `korpis upgrade --plan` to surface step 4 *before*
anything moves. This trace is the argument that it is not optional.

---

## 8. The month closes with six hours missing from the meter

The first seven traces followed one workload through its life. This one and the six after it follow
the things that surround a workload: money, data, the pipeline, the identity provider, the package,
the edge, and the extension on the critical path. Between them they cross the documents the first
seven barely touched.

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | 22:00 on the 30th: a node loses its uplink. Workloads keep running | `02-architecture.md` §4.6 |
| 2 | The agent keeps sampling from kernel counters, locally | `15-observability.md` §2 |
| 3 | The control plane stops receiving samples and reports the workload's state as `unknown` | P4, `03-state.md` §5 |
| 4 | The control plane records a gap with the cause `node unreachable` | `15-observability.md` §2 |
| 5 | 04:00 on the 1st: the link returns and the agent's journal delivers six hours of samples † | `02-architecture.md` §4.10 |
| 6 | Re-delivery is dropped by the journal cursor; a real sample supersedes the gap by appending, never overwriting † | `15-observability.md` §2 |
| 7 | The host's billing system reads the month that closed at 00:00 on the 1st | K-12, out of scope by design |

**FINDING 10: A gap and a late sample are two answers to one question. `CONFIRMED`**

Step 4 writes a gap. Step 5 delivers real measurements for the same intervals. Both are now in the
series and the document says only that samples are idempotent among themselves; it never says a gap
shares that keyspace.

The distinction the design missed is that **a gap in delivery is not a gap in measurement.** The
control plane knows one thing (nothing arrived) and the agent knows another (everything was
measured). Writing the first as though it were the second is exactly the interpolation error in
reverse: instead of inventing consumption that was never measured, it discards consumption that
was.

Patched into `15-observability.md` §2: a gap carries `(workload, resource, interval_start)` like
any other record and is superseded by a real sample for the same interval. A gap is a **claim about
the control plane's knowledge**, not a claim about the workload, and it is retracted when knowledge
arrives.

**FINDING 11: No metering period is ever closed. `CONFIRMED`**

Step 7 has no defined input. The billing system reads "the month", but nothing in Korpis says when
a month stops changing. Step 5 mutates intervals that a host may already have invoiced, and a
sample that arrives four days late mutates them again.

This is the same class of error as Finding 1. There, quota was checked without being held; here, a
period is read without being closed. In both cases the missing thing is a commitment point.

K-12 keeps Korpis out of billing, and that is still right, but a metering series with no notion of
finality is not a usable input to a billing system, it is a moving target. Korpis does not have to
issue invoices to owe its consumers a stated point after which numbers do not change.

Patched into `15-observability.md` §2: a `MeteringPeriod` closes on a declared boundary and a
declared lateness window. Samples arriving inside the window revise an open period; samples
arriving after it land in the next period as a dated adjustment, never as a silent edit of a closed
one.

**FINDING 12: The agent's metering buffer has no stated bound. `CONFIRMED`**

Step 2 assumes the agent can hold six hours. Nothing says it can hold six, or sixty, or what
happens at the limit. `14-streams.md` §3 bounds the console buffer explicitly and says what is
dropped and how the drop is marked; metering, which is the one stream with financial consequence,
has no such statement.

The absence is visible precisely because the neighbouring document does it properly.

Patched into `15-observability.md` §2: the buffer is bounded, the bound is declared, and exhausting
it produces a real gap with the cause `buffer exhausted`, which is the one case where a gap is a
statement about the workload and not only about the control plane's knowledge.

---

## 9. A customer restores one file from forty days ago

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | The customer opens a snapshot from forty days back and browses its manifest | `06-storage.md` §5.4 |
| 2 | The repository is scoped to their organization; dedup scope is the repository | `06-storage.md` §5.3 |
| 3 | They select one file; only the chunks it needs are fetched | `06-storage.md` §5.4 |
| 4 | Chunks decrypt against the workload's own data key, unwrapped by a key held outside the repository † | `06-storage.md` §5.5 |
| 5 | The read is an `Effect` and appears in the audit log | `15-observability.md` §4 |
| 6 | Restore lands in a **new** volume, which is the default presentation | `06-storage.md` §5.4 |

**FINDING 13: Browsing a manifest and reading a file are one grant and two disclosures.
`CONFIRMED`**

Step 1 reads the manifest. Step 3 reads content. `08-identity.md` §7 gates *backup contents* behind
an explicit grant, and the manifest is not contents; it is the tree. A support engineer holding
enough authority to help someone find a file can therefore enumerate every path, every filename and
every size in the volume, forty days of them, without holding the grant that gates a single byte.

For a game server the file tree is close to harmless. For a database volume, a mail spool, or a
customer's uploads directory, the tree is most of the disclosure. The design gated the expensive
half and left the informative half open.

Patched into `08-identity.md` §7 and `06-storage.md` §5.4: `backup.browse` and `backup.read` are
separate actions, browse is the weaker one, and neither implies the other. Both are audited as
sensitive reads.

**FINDING 14: A cross-tenant dedup scope is an operator decision, and the model does not say which
operator. `CONFIRMED`**

§5.3 is precise about the risk: a shared repository is a confirmation-of-file oracle, so the
default scope is the organization and sharing is "an explicit operator decision documented with
this consequence". Trace it through the reseller of §4 and the sentence stops working. In a
delegated tree every parent is an operator of its children. A reseller can put forty customers in
one repository and get a cheaper storage bill, and each customer can then test whether any other
customer holds a given file.

The decision is legitimate. What is missing is that the party who bears the consequence is not the
party who makes it, and Korpis knows which party is which.

This is Finding 7 again in a different subsystem, and the answer is the same shape: direction of
exposure decides who decides. Patched into `06-storage.md` §5.3: widening a dedup scope beyond one
organization requires the authority of the owner of every organization it will span, the same rule
§4.1 of `16-extensions.md` applies to providers, and the scope is visible to every tenant inside it
rather than being a fact about their data they cannot see.

### The same trace, run against Korpis' own backup

`18-operations.md` §6 separates tenant data from Korpis itself and lists what must be backed up.
Run steps 1 to 6 against that list and two things fall out.

**FINDING 15: Korpis' own backup can be stored in Korpis. `CONFIRMED`**

Nothing forbids putting the Postgres dump and the signing keys into a `Repository`, which is the
obvious thing to do because it is the backup system already in front of the operator. It is
client-side encrypted against a key whose reference lives in the database being backed up, so the
restore path needs the thing it is restoring.

Patched into `18-operations.md` §6: the control plane's own backup target is declared outside the
system it protects, the requirement is stated in the install preflight, and the encryption key for
it is material the operator holds, not a reference the database resolves.

**FINDING 16: "Rotate the signing key" does not say which key. `CONFIRMED`**

Restore step 2 rotates the key that capability tokens verify against. §3 of `18-operations.md`
enrolls a node by handing it "a per-node key pinned to the control plane's certificate". If those
are one key, disaster recovery ends with a fleet that cannot authenticate and an operator
re-enrolling every node by hand during the incident. If they are two, the restore is fine and
nobody wrote it down.

They should be two, and the reason is the same one that separated them everywhere else in this
design: node identity and delegated authority have different lifetimes and different revocation
stories. Patched into `18-operations.md` §6 and `08-identity.md` §6: the grant signing key and the
node identity certificate are distinct, restore rotates the first and preserves the second, and the
restore runbook of §8 says so in the step where an operator would otherwise find out.

---

## 10. The fix at 03:00, and the pipeline that runs at 09:00

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | A workload is OOM-killing under load at 03:00. It belongs to a declared set, mode `advisory` | `13-surface-cli.md` §6 |
| 2 | An on-call operator raises its memory limit through the panel. The change is an ordinary `Intent` | `03-state.md` §3 |
| 3 | The set is marked **drifted**; the drift shows in the panel and in `korpis status` | `13-surface-cli.md` §6 |
| 4 | Admission holds quota against the new figure at the moment of the change | `05-scheduling.md` §5.1 |
| 5 | 09:00: CI runs `korpis apply -f servers/ --set community-servers` under a scoped token | `13-surface-cli.md` §7 |
| 6 | The Plan shows the drifted object as a change to be reconciled or adopted | `13-surface-cli.md` §6 |

**FINDING 17: A drifted intent makes declared and enforced differ, and quota holds only one of
them. `CONFIRMED`**

Step 4 is correct in isolation: quota consumption is written in the same transaction as the intent
(§5.1, Finding 1's patch). But `advisory` mode means the *declared* file still says 4 Gi while the
*enforced* cgroup says 8 Gi, and the drift can persist for weeks because that is what advisory is
for.

Which number is the tenant's quota holding? If the declared one, the organization is over its
guarantee and nothing says so. If the enforced one, the file is not the source of truth for quota
and a `plan` that shows no change to memory is nevertheless showing a quota that will move.

The design chose `advisory` deliberately and correctly. It did not notice that advisory drift makes
the two numbers a system was built to keep identical diverge on purpose.

Patched into `05-scheduling.md` §5.1: quota is held against the **enforced** value, always, because
quota is a statement about capacity and capacity is what the kernel is actually granting. A drifted
set therefore shows the quota delta in `korpis status` alongside the drift, so the 03:00 fix that
quietly consumed a reseller's headroom is visible the same morning rather than at the end of the
month.

**FINDING 18: A non-interactive apply meets a drifted object and has no defined outcome.
`CONFIRMED`**

Step 6 says the Plan shows the change "to be reconciled or adopted". Adoption means editing the
repository, which a CI runner cannot do and should not do. Reconciliation means discarding the
overnight fix, which is precisely what `advisory` exists to prevent. §7 calls CI the good case for
a scoped token and never says what CI does when it arrives at this Plan.

Three behaviours are defensible and the document picks none, which means implementations will pick
different ones and operators will learn which by being surprised.

Patched into `13-surface-cli.md` §6: an `advisory` set with drift **fails the apply and applies
nothing**, naming each drifted object and the `Effect` that caused the drift, and `--accept-drift`
proceeds while leaving the drifted objects untouched. A pipeline that goes red at 09:00 because a
human fixed something at 03:00 is the correct outcome: the fix survives, and the disagreement
between the file and reality is escalated to a person instead of being resolved by whichever ran
last.

Step 5, added 2026-10-13 from the implementation. Building `korpis grant issue` found that the
"scoped token" of step 5 could not exist: the model had a `Token` subject and nothing that created
one, and a link-form grant, the only bearer form described, would have kept working for its whole
90-day life after revocation. §3.7 of `08-identity.md` now issues a grant to a token created to
hold it, authenticated online. Running the step end to end then stopped one call later: the
token, scoped to the project, could read no organization, so the CLI could not resolve the
project's name in the file it was applying. Names now resolve from the caller's own grant anchors
as well (§2 of `13-surface-cli.md`), and `crates/korpis-cli/tests/cli/grants.rs` runs the step.

Steps 1, 3, and 6, added the same day. Asked what evaluates "the set is marked drifted" and with
what information, the model had no answer: `managed_by` and the per-set mode appeared only in
§6 of `13-surface-cli.md`, and no object held either. `DeclaredSet` (§3.3 of `01-model.md`) now
holds the mode, the workload names its set and each intent the set that declared it, and the
control plane derives drift from those two pointers at declaration and in every status read, so
the panel's 03:00 change is drift without anything having to mark it.
`crates/korpis-server/tests/api/sets.rs` runs steps 1, 3, and 6 against both modes, and
`crates/korpis-cli/tests/cli/sets.rs` runs the whole trace through the CLI: the 09:00 apply exits
with `CONFLICT` naming the 03:00 change and its effect, `--accept-drift` applies the rest, and a
member removed from the files is deleted only by the `apply --plan` that names its retire plan.

---

## 11. The moderator loses the role

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | A grant names *the `@moderator` role in guild 456* as its subject | `08-identity.md` §5 |
| 2 | A moderator uses it, and issues an attenuated child grant to a helper: restart one server, 7 days, as the parent's `delegation` policy allows † | `08-identity.md` §3.2 |
| 3 | They also open a console through a relay; a 120-second capability token is issued with a sealed route to the node, renewed, and checked by relay and agent † ‖ | `08-identity.md` §6; `02-architecture.md` §3.1 |
| 4 | They also create a share link, valid 24 hours, token in the fragment | `08-identity.md` §6.1 |
| 5 | The guild owner removes them from `@moderator` | Discord, outside Korpis |
| 6 | §5 says losing the role removes the authority with no deprovisioning step | `08-identity.md` §5 |

**FINDING 19: Losing an external role produces no event, so nothing cascades. `CONFIRMED`**

Step 6 is true of exactly one of steps 2, 3 and 4. The console token stops renewing within 120
seconds, which is the bound §6 promises and it holds.

The other two do not. §3.5 makes revocation cascade to the entire subtree, and that machinery runs
when a grant is revoked. Nothing was revoked here. Discord does not notify Korpis that a role
membership ended; the role assignment simply stops appearing in the next signed interaction, and if
that person never interacts again, Korpis never observes anything at all. So the helper's seven-day
child grant and the twenty-four-hour share link both keep working, issued by an authority that no
longer exists, and the audit trail shows a chain rooted in a subject who cannot use it.

"No deprovisioning step" was written as a feature and it is one. It is also the reason there is no
event, and the design read one half of that and not the other.

Patched into `08-identity.md` §5 and §3.5: an `ExternalIdentity` binding is **re-verified on a
declared interval**, not only when its holder happens to interact, and a binding that fails
re-verification enters `unverified`, which suspends every grant rooted in it and cascades exactly
as revocation does. Suspension rather than revocation because an identity provider being
unreachable must not silently destroy a delegation tree; `unverified` is visible, dated, and
reversible, and it fails closed for authority while failing loudly for the operator. The
re-verification interval is the honest analogue of token lifetime: it is the bound on how long
authority outlives its source, it is stated, and it is configurable rather than convenient.

Building the revocation list re-ran step 3 against the question a trace does not ask: what refuses
the console token offline, where, and with what information. The relay and the agent do, from the
last list the control plane signed with the token key, which carries every grant whose own row is
revoked or suspended (§6.3 of `08-identity.md`). The suspension of this trace is not such a row:
a binding that fails re-verification suspends the grants rooted in it by derivation, as an
organization's suspension does (§2.1, §3.5), so the console token of step 3 is refused at its next
renewal, within the 120 seconds §6 promises, and not sooner. The list could do better, and how is
question 7 of §10 of `08-identity.md`, settled with bindings and organization suspension in
Phase 2. Building the data path then answered the trace's other half: a grant that *is* revoked
now ends the console of step 3 while it is open, because the node holding the session checks each
new list against the session's chain, and the token's renewal is carried on the session itself,
judged by the relay and the node in turn (§3.1 of `02-architecture.md`).

Running the relay as its own process asked the algebra question of step 3 once more: with what
information does a relay that holds no database refuse the revoked token? With the list its feed
sent, believed only once it verifies against the token keys the same feed sent, and pushed within
about two seconds of the control plane computing it (§3.1 of `02-architecture.md`). A relay whose
feed was lost keeps the last list it believed, so a revocation it missed is bounded by one token
lifetime, the bound §4.6 of `02-architecture.md` already states for the agent, and the node
refuses the session's next renewal by its own list regardless. The same build found that a node's
proof to a relay could be replayed to another relay, since relays share no nonce memory; the
proof now names the relay it was made for.

‖ Added 2026-10-14, when `korpis logs`, the first command to open a session, was built.

**FINDING 29: A client who may not read the node could not reach the workload. `CONFIRMED`**

The relay routes a session by the node the client names, and §3.1 of `02-architecture.md` had the
client name "the node the API reports it placed on". The API reports a placement's node only to a
caller who may read the node, and the moderator of step 3 holds a console grant on one server and
nothing on the host's machines. So the moderator was told nothing to name, the relay, which holds
no database, had nothing to look up, and the console of step 3 could not be opened at all. Every
test had opened sessions as the root organization, which may read every node, and the trace's
step read as possible because nobody asked with what information the client fills in the node.

**Resolution.** The gateway, which knows the placement when it mints the token, returns with it a
route: the node, sealed under a key derived from the token key and a fresh nonce, bound to the
workload. The relay opens it with that key, carried in its feed beside the token keys, and the
moderator learns nothing about the host's machines, not even which of their own servers share one.
→ patched into `02-architecture.md` §3.1, `08-identity.md` §6, and `17-security.md` §8

Building console input asked the algebra question of step 3's other half: when the moderator
types `stop`, what records that they did, and with what information is it believed? The node
that carried the session journals it as the moderator's act under the grant their token was
minted from, and the control plane records it so only if that grant is the moderator's and
permitted the input when it happened (§6 of `14-streams.md`). After step 5 the effect still names
them, which is the point: the record is of who typed, under the authority they then held. What
the check cannot do is stop a compromised node from putting words in the mouth of a moderator
whose session it carried; it bounds whom a node can implicate, not what it claims.

---

## 12. A recipe install fails at step four of six

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | An `Intent` names a recipe by digest, resolved once through the lockfile | `09-recipes.md` §8 |
| 2 | The Plan contains a step: run the recipe's install | `05-scheduling.md` §3.1 |
| 3 | The install DSL runs six steps: fetch, verify, extract, `steam.app`, template, chmod | `09-recipes.md` §4 |
| 4 | Step four calls an extension-provided step provider, pinned in the lockfile as `<publisher>/<step>`, which is circuit-broken † | `16-extensions.md` §4.1, §5 |
| 5 | The Plan step fails. The workload's state is `partial`, with the failed step attached | `05-scheduling.md` §3.1 |
| 6 | Resources created by completed steps are retained and named in the observation | `05-scheduling.md` §3.1 |
| 7 | The provider recovers; the plan resumes from the last completed step | `05-scheduling.md` §3.1 |

**FINDING 20: The install DSL's steps and a Plan's steps are different granularities, and the
resumability guarantee stops at the boundary. `CONFIRMED`**

Step 7 resumes from the last completed **Plan** step, and the whole install is one of those. So a
resume re-runs all six DSL steps, including a forty-gigabyte SteamCMD fetch that had already
succeeded, and the guarantee of §3.1 that a failed plan is "resumable, not restarted" is true of
the Plan and false of the thing the operator is actually waiting on.

Step 6 makes it worse rather than better. The partial install is retained, so the re-run of DSL
steps one to three is now operating on a directory that already has content, and §3.1's idempotence
guarantee was written for Plan steps, not for install verbs. `extract` over an existing tree and
`template` over an already-rendered file are the two most obvious ways to produce a workload that
starts and is subtly wrong.

Patched into `09-recipes.md` §4 and `05-scheduling.md` §3.1: install steps carry the same three
guarantees as Plan steps, ordered, individually idempotent, and resumable from the last completed
one, with progress recorded durably per install rather than per plan. The DSL's verbs are a closed
set precisely so that each can state its idempotence, which is a property this design already
needed and had only asserted one level up.

---

## 13. The edge fails, not the node

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | Two hundred workloads are exposed through one `stable` address on an edge failover group † | `07-networking.md` §3.1 |
| 2 | Thirty of them are hibernated and wake on connection | `22-first-party.md` §6 |
| 3 | The edge's forwarding plane stops passing traffic. Its host is up and its agent is healthy | `18-operations.md` §5 |
| 4 | Every node holding a workload is up, holds a valid lease, and reports `running` | `05-scheduling.md` §7 |
| 5 | The control plane observes nothing wrong, because nothing it observes is wrong | P4, `03-state.md` §5 |

**FINDING 21: The edge is the only component in the design that holds no lease. `CONFIRMED`**

Every other authority in Korpis is fenced. Agents hold leases with epochs, the control plane fences
on restore, migration cuts over at one instant under a lease. The edge forwards traffic for two
hundred workloads and holds nothing, so there is no mechanism by which a half-failed edge can be
detected as failed, and no mechanism by which a replacement can know the old one has stopped.

§11.2 of `07-networking.md` admitted the shape of this honestly: `stable` trades Pterodactyl's
distributed failure mode for a concentrated one. It did not follow the admission through to the
consequence, which is that the concentrated component is the one component with no health contract.

Step 5 is the part that should have been caught earlier. Every observation in the system is correct
and the service is down, which is the exact failure P4 was written to prevent, occurring in the one
place P4 was never applied.

Patched into `07-networking.md` §3.1 and `18-operations.md` §5: an edge holds a lease with an epoch
like any other data-plane component, and its liveness is **measured through the forwarding path**
rather than from the health of the process that owns it. Failover advances the epoch, and the
detection interval is declared, because an operator choosing a concentrated failure mode is
entitled to know how long the concentration lasts.

**FINDING 22: A hibernated workload's wake trigger lives in the failed component. `PLAUSIBLE`**

Step 2 crossed the same boundary and lands differently. Waking on connection requires something to
observe the connection, and that something is the edge. With the edge half-failed, thirty workloads
sit in `hibernated`, which is an accurate state, while being unreachable, which nothing reports,
because §6 of `22-first-party.md` deliberately never displays a hibernated workload as running and
therefore never displays it as down either.

Marked `PLAUSIBLE` rather than `CONFIRMED` because Finding 21's patch fixes the mechanism: an edge
with a measured forwarding path fails visibly, and its dependents become `unsatisfiable` with the
edge named. Recorded anyway, because it is the case that shows why the measurement has to be
through the forwarding path and not from the process.

Running relays as their own process put Finding 21's question to the relay tier on 2026-10-14:
what knows that a relay has failed, and with what information? Not one component, and it is
better that way. The replica feeding a relay records each look at the feed, so `ListRelays`
shows a relay whose feed went quiet as not connected within ninety seconds; a node's data
connection to it drops and is dialled again with backoff; a client whose session ended opens the
next through another relay `ServerInfo` lists. Nothing fails over because no relay is primary,
and nothing is concentrated the way an edge concentrates addresses: every node attaches to every
relay, so a failed relay costs the sessions it carried and nothing else (§5 of
`18-operations.md`). What the measurement does not see is a relay whose feed is healthy and whose
listener is not; the balancer's health check in front of the relays is the measurement for that.

---

## 14. A provider answers, slowly

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | A workload declares an `ingress` endpoint needing a DNS record | `07-networking.md` §3.2 |
| 2 | The Plan's step calls the DNS provider extension, under a deadline, down the stream the extension opened as `extension:<id>` with a credential its node vouched for ¶ | `16-extensions.md` §3.2, §3.3, §5; `08-identity.md` §9.5 |
| 3 | The provider answers at 90% of the deadline, every time, for every workload in the fleet | `16-extensions.md` §5 |
| 4 | It never fails, so the circuit breaker never opens | `16-extensions.md` §5 |
| 5 | Every create in the fleet now takes the deadline, and the reconciler's queue grows | `05-scheduling.md` §7 |

**FINDING 23: The circuit breaker measures failure, and the failure mode is success. `CONFIRMED`**

§5 is careful about the two obvious cases: a provider that is unreachable and a provider that fails
consistently. Both are handled, and the reasoning for each is stated. Neither describes step 3,
where the provider is available, correct, and slow enough to consume the fleet's throughput while
producing no error to trip anything.

This is the classic degraded-dependency shape and it is worth stating plainly: **a deadline bounds
one call and says nothing about aggregate cost.** Two hundred calls at 90% of a five-second
deadline is fifteen minutes of reconciler time that no document accounts for, spent on third-party
code, with every observation reporting success.

Patched into `16-extensions.md` §5: provider latency is measured and published per provider, the
circuit breaker trips on sustained latency as well as on failure, and provider concurrency is
bounded per provider rather than per call so that one slow dependency cannot occupy the fleet's
reconciliation capacity. `15-observability.md` §5 gains the provider-degraded event, because the
operator whose creates got slow this morning should not have to infer the cause from a graph.

¶ Added 2026-10-13, while building the extension contract. Step 2 could not happen as written:
the extension runs on a node that accepts no inbound connection, so nothing could call it, and it
had no way to prove it was the subject its grants were issued to. The extension now dials out and
is called down its own stream (§3.3 of `16-extensions.md`), and authenticates with a short-lived
credential its node obtains for it while it runs there (§9.5 of `08-identity.md`). Its subject was
keyed by name, so one extension installed into two organizations would have been one subject; it
is keyed by the installation (§3.2 of `01-model.md`). The events the provider's slowness produces
now have a log that says what an event is and how a consumer keeps its place (§5 of
`15-observability.md`). Building the credential then found two more gaps. The driver's door
(§6.1 of `02-architecture.md`) allowed no mount but volumes, so the socket could not reach the
extension at all until the door named the workload's own identity directory as the one other
mount, read-only. And the identity root was readable by every user on the host, so any local
process could have asked a workload's socket for its credential; it is now the supervisor's alone
(§6 of `02-architecture.md`). Building install and uninstall found one restatement the re-keying
of the subject had missed: the CLI still wrote an extension holder as `extension:NAME`, which
named no installation; it is `ext_…` now (§7 of `13-surface-cli.md`). It also found that §9 of
`16-extensions.md` called an upgrade a plain rolling update, which would let a new version arrive
with needs nobody approved; question 6 there records how an upgrade is approved instead.
Building the call path found that step 2 assumed the control-plane replica running the step holds
the provider's stream; with several replicas and no leader (§5 of `18-operations.md`) it rarely
does. Calls are now queued in Postgres and claimed by the replica that holds a stream, and the
bounded concurrency step 5 relies on is enforced by that claim, per stream (§3.3 of
`16-extensions.md`). Building the breaker answered the algebra question of step 4 (what judges
the breaker, where, with what information): each caller, after its call finishes, from the
queue's own record of the provider's last twenty calls, under the breaker's row lock, so the
provider of step 3 now trips it on latency and step 5 is cut short (§5 of `16-extensions.md`).
It also found that §5 declared failure behaviour for two of the eight provider types while
forbidding a guess for any; all eight are declared there now. The upgrade question 6 of §9 of
`16-extensions.md` recorded is built: the provider of step 3 can be replaced by its next version
with the grants its needs still ask for kept, its streams draining, and the same subject throughout
(§7 of `13-surface-cli.md`).
Writing the SDK the provider of step 2 is built with found that step 2 could still not happen
from inside a container: the egress default of §6 of `07-networking.md` denied every workload the
control plane, the one destination an extension exists to reach, and nothing told a workload
where the control plane is or how to trust it. The default now opens the API's address to an
installed extension's workload, decided by the control plane and applied by the node, and the
identity socket answers with the address and the CA its node pinned (§9.5 of `08-identity.md`).
The call of step 2 now exists in the contract: an endpoint's records are sent whole to the DNS
provider, which makes its zones agree and refuses names outside them (§3.5 of
`07-networking.md`), so a retry after the slow provider of step 3 times out converges rather than
duplicating records.
Step providers, the other provider of the install path, now answer with content-addressed
artifacts the node fetches by hash, so a provider that fails at step four of an install (§12) can
be retried without any risk to the volume: no provider can write it (§4 of `09-recipes.md`).
Backup targets, the third provider type built, issue scoped access per job rather than holding a
node's data, and building them found that a store unable to scope access has no path yet once its
credential is a provider's (question 7 of §9 of `06-storage.md`).

**One thing this trace did not find**, and it is worth recording because it was the suspicion that
motivated the trace: the tenant boundary holds. A provider installed by one tenant is bounded by
the scope of the grants it was installed with (§4.1 of `16-extensions.md`), so a slow
tenant-installed provider degrades that tenant's own reconciliation and nobody else's. The finding
above applies to providers installed at the operator level, which is the case §2 calls "core uses
the same mechanism", and that is the price of having made core and extensions the same thing.

---

## 15. Findings

| # | Finding | Trace | Patched into |
|---|---|---|---|
| 1 | Quota is checked without being held, concurrent creates overshoot | §1 | `05-scheduling.md` §5.1 |
| 2 | A single workload's Plan has no stated step atomicity | §1 | `05-scheduling.md` §3.1 |
| 3 | Streams do not migrate with their workload; offsets would break | §2 | `05-scheduling.md` §8, `14-streams.md` §4 |
| 4 | Console reader across cutover | §2 | correct, given 3 |
| 5 | A `stable` address with no live workload has undefined behaviour | §3 | `07-networking.md` §3.1 |
| 6 | Quota inheritance says neither allocation nor usage, decides Bet 4 | §4 | `05-scheduling.md` §5.1 |
| 7 | Who approves an extension inside a delegated organization | §4 | `16-extensions.md` §4.1 |
| 8 | Egress enforced by the party being contained | §5 | `17-security.md` §9.1 |
| 9 | Capability tokens outlive a restored database, revoked grants resurrect | §6 | `03-state.md` §8, `08-identity.md` §6, `18-operations.md` §6 |
| 10 | A gap and a late sample are two records for one interval | §8 | `15-observability.md` §2 |
| 11 | No metering period is ever closed, so an invoice has no defined input | §8 | `15-observability.md` §2 |
| 12 | The agent's metering buffer has no stated bound | §8 | `15-observability.md` §2 |
| 13 | Browsing a backup manifest is not gated like reading one | §9 | `08-identity.md` §7, `06-storage.md` §5.4 |
| 14 | A cross-tenant dedup scope is decided by the party who does not bear it | §9 | `06-storage.md` §5.3 |
| 15 | Korpis' own backup can be stored inside Korpis | §9 | `18-operations.md` §6 |
| 16 | "Rotate the signing key" does not say which of the two keys | §9 | `18-operations.md` §6, `08-identity.md` §6 |
| 17 | A drifted intent makes declared and enforced differ; quota holds one | §10 | `05-scheduling.md` §5.1 |
| 18 | A non-interactive apply meeting drift has no defined outcome | §10 | `13-surface-cli.md` §6 |
| 19 | Losing an external role produces no event, so nothing cascades | §11 | `08-identity.md` §3.5, §5 |
| 20 | Install steps and Plan steps are different granularities | §12 | `09-recipes.md` §4, `05-scheduling.md` §3.1 |
| 21 | The edge is the only component holding no lease | §13 | `07-networking.md` §3.1, `18-operations.md` §5 |
| 22 | A hibernated workload's wake trigger lives in the edge | §13 | fixed by 21 |
| 23 | The circuit breaker measures failure, and the failure mode is success | §14 | `16-extensions.md` §5, `15-observability.md` §5 |
| 24 | A deleted workload's identity has no moment it is deleted | §18 | `01-model.md` §3.3, `02-architecture.md` §4.8 |
| 25 | An organization without its own trust policy had no policy at all | §4 | `09-recipes.md` §7.1 |
| 26 | A restored cursor older than a node's journal waits forever | §6 | `02-architecture.md` §4.10, `03-state.md` §8 |
| 27 | A relay removed after the backup returns with it | §6 | `18-operations.md` §6 |
| 28 | The last segment of a retired workload's console was refused | §18 | `14-streams.md` §4 |
| 29 | A client who may not read the node could not reach the workload | §11 | `02-architecture.md` §3.1, `08-identity.md` §6 |
| 30 | A retained volume was listed by nothing | §18 | `06-storage.md` §7, `01-model.md` §3.6 |
| 31 | A rollback under a running workload replaces what it has open | §19 | `06-storage.md` §4, §7 |
| 32 | Two repositories at one place would prune each other | §20 | `06-storage.md` §5.3, §5.5 |
| 33 | A nightly backup had nothing to run it | §20 | `20-roadmap.md` Phase 1 |
| 34 | Replacing a volume needs room for two copies of it | §21 | `06-storage.md` §5.4 |
| 35 | A prune could remove the backup a restore is reading | §21 | `06-storage.md` §5.5 |

**What the second seven traces cost, and what they bought.** They took a paragraph each to write
and produced fourteen findings, four of which (11, 16, 19, 21) are the kind that are discovered in
production by a customer, an auditor, or an outage rather than by a maintainer.

Findings 1, 6, 9, 16, 19, and 21 are the ones that would have cost real money to discover late. An
overshot quota is a support ticket. An unstated overcommit model is a reseller telling their
customers something Korpis does not do. A resurrected grant is a security incident with no audit
trail. Rotating the wrong key turns a recovery into a fleet-wide re-enrollment during the incident.
An external role that is removed without cascading is authority nobody can see. An unfenced edge is
two hundred workloads down while every observation reports healthy.

All six were found by the same method, following one concrete story across a boundary that two
documents each believed the other was holding. None of them is subtle in hindsight, which is the
point: they were invisible per document and obvious per trace.

---

## 16. An external review, and what it found

The fourteen traces above are one method: follow a concrete story until it crosses a boundary
nobody owns. In August 2026 the specification was also read by a different model with a different
method, which attacked the **algebra** inside each document rather than the seams between them. It
found nineteen things. Seventeen were real and are patched; one (R1) had been closed hours earlier
by Finding 19 above and the reviewer could not see it; one (R18) was already partly answered.

> Corrected 2026-10-12. This said sixteen were real and two partly answered, which the table below
> did not support: it marks seventeen real.

Recording it here, in full, because §2 of `01-model.md` set the rule that corrections stay visible,
and because the split between what the two methods caught is the most useful thing in this
document.

| # | Finding | Verdict | Patched into |
|---|---|---|---|
| R1 | Set-subject grants: membership lives in no link of the chain, so children outlive it | **already closed**, and complemented | `08-identity.md` §5.1 |
| R2 | `max_uses` cannot be enforced by an offline verifier, so it was a displayed limit nothing held | **real, worst of the set** | `08-identity.md` §6.2 |
| R3 | `remaining_uses` is racy at issue time | real | `08-identity.md` §6.2 |
| R4 | Cascade revocation: derived or materialized was never chosen | real | `08-identity.md` §3.5, §6.3 |
| R5 | Per-request authorization cost, and cache invalidation vs immediate revocation | real | `02-architecture.md` §5.1 |
| R6 | Preempting a plan that is already applying | **real** | `03-state.md` §7 |
| R7 | Power state inside the intent body turns config history into a power log | **real** | `01-model.md` §3.3 |
| R8 | Effect PK and partition key fed by agent clocks | real | `03-state.md` §4 |
| R9 | Auto-approval hides a policy engine inside the word "non-disruptive" | real | `01-model.md` §3.3 |
| R10 | Proto3 canonical JSON omits defaults, inverting the immutability claim | **real** | `03-state.md` §3.1, `10-api.md` §4 |
| R11 | The epoch fence advances past a watermark that is itself stale | **real** | `03-state.md` §8 |
| R12 | A fenced agent should fail static, not stop tenant workloads | real, default now stated | `03-state.md` §8, `02-architecture.md` §4.5 |
| R13 | Revoking one break-glass token required rotating every key | real | `08-identity.md` §6.3 |
| R14 | `LISTEN`/`NOTIFY` under a transaction-pooling PgBouncer; link IP binding vs CGNAT | real, both | `18-operations.md` §5, `08-identity.md` §6.1 |
| R15 | K-3 and K-9 cannot both hold without per-driver enforcement declarations | **real** | `04-runtimes.md` §4.1 |
| R16 | A volume is a filesystem under one tier and a block device under another | **real** | `04-runtimes.md` §4.1, `06-storage.md` §3.1 |
| R17 | Devices are missing from the capability declaration; Firecracker has no passthrough | real | `04-runtimes.md` §4.1 |
| R18 | Whether `stable` puts a hop in front of latency-sensitive traffic | partly answered, now explicit | `07-networking.md` §3.1 |
| R19 | Service discovery is parked in a later phase than the requirement | real | `20-roadmap.md` §4 |

R6 and R19 came back during the Phase 0 spec sync (2026-10-13), each a correction that had reached
the documents and not the build. R6: cancelling a plan nodes had begun still answered "not yet",
in the phase that owed it; it is built, and §7 of `03-state.md` now says how a node learns of it
between steps. R19: the roadmap had moved the decision to Phase 1 and still left the schema there,
after §5 of `07-networking.md` had made the decision; the schema is now in Phase 0's, and Phase 1
owes only the resolver.

### What the two methods caught, and why it differs

The traces found **seam** defects: two documents each believing the other held something. The
review found **algebra** defects: a single document asserting a property its own definitions cannot
support. Almost nothing appears on both lists, and the one thing that does, R1 and Finding 19, was
reached from opposite directions on the same day.

R2 is the clearest example and the most uncomfortable. Trace §1 ends by issuing a share grant and
never asks who holds its limits, and `max_uses` was one of them. A trace verifies that each
step has an owner; it does not verify that a step is *possible*. The document that lectures every
other document about displaying only what is enforced was displaying a limit nothing enforced, and
it took someone reading the definitions rather than following the story.

R11 is the same shape. "Advance past the watermark" reads correctly in sequence and is
arithmetically insufficient, and no trace catches that, because a trace confirms a step exists
rather than checking its maths.

The practical conclusion is written into `CLAUDE.md`: **traces and algebra review are different
instruments and a document needs both.** A change that adds a field, a condition, or a limit gets
the algebra question asked of it directly, which is "what evaluates this, where, and with what
information", and the answer has to name a component that actually has that information.

### What was not accepted

Two claims did not survive checking, and saying so matters as much as the rest.

The review argued that the gateway becomes "the single bottleneck" as well as the single
authorization point. It is the single authorization point by design (P6), and that part stands. It
is not a bottleneck: the gateway is stateless and horizontally scaled with no leader (§5 of
`18-operations.md`), so the cost is throughput, not a serialization point. The invalidation gap
underneath the argument was real and is fixed; the conclusion drawn from it was not.

The review also suspected that `stable` puts a permanent hop in front of UDP game traffic. §3.1 of
`07-networking.md` already had the edge co-resident with kernel-level forwarding for exactly this
reason. What was genuinely missing was the word "default", which is now there, along with the
statement that a remote edge is a deliberate choice with a stated cost.

> Superseded 2026-10-12 by S39 in §17. The co-resident default defended here was itself wrong: a
> co-resident edge stops forwarding when its workload migrates away, so `stable` now requires an
> edge failover group, and the latency-first option is the separate, honestly labelled `node_bound`
> mode. The reviewer's suspicion was closer to right than this paragraph allowed.

The reviewer flagged three findings as unverifiable because they could not read
`02-architecture.md`, `17-security.md`, or this document. Checked against those files: R5 was
correct, §5 did specify no caching design at all; R11 was correct; R18 was half correct as above.

---

## 17. A second external review, and what it found

In October 2026 the specification was read a third way. Claude, the model this repository says it
was written with, read all twenty-four documents in a session with the maintainer: four readings
partitioned by subsystem, each asked for contradictions, algebra defects, unowned steps, broken
cross-references, and evidence errors, followed by a direct reading of the core documents. Every
finding ranked high was then checked against the files themselves, and every claim about another
project against its primary source (the GitHub advisory and issue APIs, and runc's source). Nothing
below is recorded on a reviewer's word alone.

It found seventy-nine things, and all seventy-nine are patched. The table records each with its
verdict and where it was patched, grouped by subsystem: the state core first, then identity, the
data path and runtime, and last governance and evidence.

| # | Finding | Verdict | Patched into |
|---|---|---|---|
| S1 | Plans had no carrier in the agent protocol; `plan.cancel` and the migration phases were imperative messages the protocol did not contain | **real** | `02-architecture.md` §4.8, §4.9; `03-state.md` §7; `05-scheduling.md` §8 |
| S2 | R7 was applied to one paragraph: Start was still "an intent change", restart used a field that no longer existed, and nothing carried `Power` to a node | **real** | `01-model.md` §3.3; `02-architecture.md` §4.1, §4.8 |
| S3 | `Workload.intent` moved at declaration, so nodes converged toward changes nobody had approved | **real, bypassed P5** | `01-model.md` §3.3; `03-state.md` §3.2; `02-architecture.md` §4.3, §4.8 |
| S4 | The step key `(workload, intent_version, step)` collided for every plan without a new intent; failed plans were "fresh" in one document and resumable in two others | **real** | `02-architecture.md` §4.7, §9; `03-state.md` §6 |
| S5 | Deleting a workload had no mechanism distinguishable from forgetting it | real | `02-architecture.md` §4.8 |
| S6 | Node-originated effects had no journal, acknowledgement, or deduplication, and the same-transaction claim was made for events on other machines | **real** | `02-architecture.md` §4.10; `03-state.md` §1, §4; `01-model.md` §3.3 |
| S7 | Lease exclusivity depended on an edge-triggered revoke arriving and on node clocks | **real** | `02-architecture.md` §4.5; `05-scheduling.md` §8 |
| S8 | A fenced agent kept running, stopped, and stopped, in three places; the `on_expiry` default was global in two documents and "wrong" in a third; no field existed | **real** | `02-architecture.md` §4.5; `01-model.md` §3.3; `06-storage.md` §2, §7; `03-state.md` §8; `18-operations.md` §5 |
| S9 | Restore still used the pre-R11 epoch arithmetic in one document, and nothing fenced intent versions, so a restore rolled configuration back silently and reissued version numbers | **real** | `03-state.md` §8; `18-operations.md` §6 |
| S10 | Grant-cache invalidation rode a queue that delivers each job to one worker, and re-introduced the subtree walk R4 removed | **real** | `02-architecture.md` §5.1 |
| S11 | Metering was outside PostgreSQL in one document and inside it in another; "append-only" and "overwrites"; "approximately zero" load | real | `03-state.md` §2, §5, §9; `15-observability.md` §2, §7 |
| S12 | Streams were promised to outlive the node without offload being on | real | `01-model.md` §3.6; `14-streams.md` §4 |
| S13 | The scheduler called `Prepare` on candidate nodes through a channel that did not exist | **real** | `05-scheduling.md` §2.1; `02-architecture.md` §4.2 |
| S14 | No action for approving a plan, declaring configuration, exec, or reading a secret | real | `10-api.md` §5 |
| S15 | Recorded denials were an unbounded, unmetered write path | real | `10-api.md` §10; `15-observability.md` §4 |
| S16 | `01-model.md` had not absorbed later decisions: the flat `Quota`, `Endpoint` without `stable`, plan and observation states | real | `01-model.md` §3.1, §3.3, §3.6 |
| S17 | R9's disruption classes and stream redaction relied on recipe fields `09-recipes.md` did not define | real | `09-recipes.md` §5, §5.1 |
| S18 | Resolved open questions were left open, and questions routed to `15-observability.md` were dropped there | real | `01-model.md` §7; `02-architecture.md` §10; `03-state.md` §10; `05-scheduling.md` §10; `15-observability.md` §7 |
| S65 | The agent protocol's "N-2" window did not survive an LTS-to-LTS upgrade | real | `10-api.md` §4; `18-operations.md` §4; `19-governance.md` §5 |
| S78 | The `Quota` invariant said every dimension was kernel-enforced, which no count can be, and nobody owned the monthly egress budget | real | `01-model.md` §3.1 |
| S79 | One filesystem worker per tenant could not exist (a stopped container has no namespace) and would have authorized sibling access by path | **real** | `02-architecture.md` §6 |
| S20 | Nothing said who may delegate or who may revoke; both readings broke the document's own canonical example | **real** | `08-identity.md` §3.2, §3.5; `01-model.md` §3.2; `10-api.md` §5 |
| S21 | The Discord client "exchanged" a user's identity for that user's token, which extensions may not hold, with no signature, replay, or narrowing rule | **real, worst of the identity set** | `08-identity.md` §5.2; `12-surface-discord.md` §1; `16-extensions.md` §7 |
| S22 | The chat document kept role grants valid through an outage that the identity document suspends them for | real | `08-identity.md` §5.1; `12-surface-discord.md` §9 |
| S23 | Re-verifying a role cannot notice one member leaving it, so that member's delegations outlived them | **real** | `08-identity.md` §5.1; `01-model.md` §3.2 |
| S24 | "No default access to a child's files" could not be computed: containment carried the host's root grant into every tenant | **real** | `08-identity.md` §3.3 |
| S25 | Extensions acted under their own grant for any resource an invoker named, and iframe tokens were not intersected with the viewer | **real** | `16-extensions.md` §5 |
| S26 | Step providers had no installer, namespace, or pinning, and provider-fetched content broke "one digest, same bytes" silently | real | `16-extensions.md` §4.1, §5; `09-recipes.md` §4, §8; `22-first-party.md` §4 |
| S27 | `requires_approval_by` had no order a verifier could compute, and permitted self-approval | real | `08-identity.md` §3.6 |
| S28 | Rate limits per grant multiplied under attenuation, pooled a whole role, and left login unlimited | real | `10-api.md` §10 |
| S29 | A bearer link could be posted into a public channel; `max_uses` counted session renewals; the share screen hid the cap | real | `08-identity.md` §6.1; `11-surface-web.md` §6; `12-surface-discord.md` §6 |
| S30 | The offline revocation list held token identifiers, so revoking an ancestor did not reach its descendants offline | real | `08-identity.md` §6.3 |
| S31 | Operator-only config fields named no action and could be overridden by editing the rendered file; templates did not encode values | **real** | `09-recipes.md` §5, §5.2, §6 |
| S32 | The only lever for "suspend this customer for non-payment" was irreversible, cascading revocation | real | `08-identity.md` §2.1; `01-model.md` §3.1; `10-api.md` §5 |
| S33 | No bootstrap of the first grant, no lockout recovery, no account recovery, no session lifetimes, no `exec` action | real | `08-identity.md` §9.1 to §9.4; `10-api.md` §5 |
| S34 | Three definitions of `IdentityBinding`; role-to-template binding described two ways; templates impossible to locate | real | `08-identity.md` §4, §5; `01-model.md` §3.2; `12-surface-discord.md` §2 |
| S35 | Two different lists of irreversible actions, one enforced by the bot it said never adjudicates | real | `10-api.md` §5; `08-identity.md` §5.2; `12-surface-discord.md` §3 |
| S36 | Waking a hibernated workload had no subject and no grant, and any port scan could trigger it | real | `22-first-party.md` §6 |
| S19 | Four documents sent clients directly to nodes that, by design, have no inbound port | **real** | `02-architecture.md` §3.1, §4.2; `14-streams.md` §4; `11-surface-web.md` §8; `13-surface-cli.md` §8; `08-identity.md` §6; `17-security.md` §8 |
| S37 | Backups needed a repository-wide key and credential on every node, against the document that said nodes hold neither | **real** | `06-storage.md` §5.2, §5.3, §5.5; `02-architecture.md` §7; `17-security.md` §7, §9; `14-streams.md` §9 |
| S38 | IOPS reservations and I/O shares were displayed with no mechanism that could hold them, and ZFS cannot attribute writes | **real** | `05-scheduling.md` §5.2, §5.3; `06-storage.md` §3; `18-operations.md` §2 |
| S39 | The default co-resident edge stopped forwarding after a migration, so `stable` addresses did not survive the move they were for | **real** | `07-networking.md` §3, §3.1; `18-operations.md` §2, §5; `01-model.md` §3.6 |
| S40 | File access, SFTP, and single-file restore had no mechanism for block-device volumes, and the obvious one parsed hostile images on the host | **real** | `04-runtimes.md` §4.4; `02-architecture.md` §6; `06-storage.md` §5.4 |
| S41 | "No window" at cutover, contradicted by BGP convergence, multi-edge updates, and connection tracking | real | `07-networking.md` §10; `05-scheduling.md` §8 |
| S42 | Nothing said what address a workload sees through an edge, and NAT would have erased every player's | real | `07-networking.md` §3.4; `01-model.md` §3.6 |
| S43 | "A NAT-bound node runs game servers perfectly" was true only behind an edge, and no filter knew the difference | real | `07-networking.md` §9; `05-scheduling.md` §2.1 |
| S44 | Egress was default-deny in the threat model and default-allow in its owning document; inbound on IPv6 was unstated | real | `07-networking.md` §4, §6; `17-security.md` §2, §10 |
| S45 | No document owned workload DNS, and the default policy blocked the resolvers of two major clouds | real | `07-networking.md` §5.1 |
| S46 | `microvm` was said to stop "most side channels", which SMT-sibling leaks contradict | real | `17-security.md` §5, §13 |
| S47 | Inbound bandwidth was sold as guaranteed, which no node can enforce | real | `07-networking.md` §7; `05-scheduling.md` §5.2 |
| S48 | `guaranteed` was sold as "dedicated vCPU" with nothing that dedicated a core | real | `05-scheduling.md` §5.2; `17-security.md` §13 |
| S49 | Concurrent backup and prune had no coordination owner; the dedup open question was framed wrongly | real | `06-storage.md` §5.5, §9 |
| S50 | Nothing allocated user-namespace ranges, so a migrated workload's files could arrive owned by someone else | real | `04-runtimes.md` §4.5 (ranges per node, corrected 2026-10-13) |
| S51 | A live-state migration could choose a destination its snapshot cannot be restored on | real | `04-runtimes.md` §4.1 |
| S52 | `statfs` does not report btrfs quotas; ZFS `refquota` has no inode limit; the LVM-thin row contradicted itself | real | `06-storage.md` §2, §3 |
| S53 | "A container and a microVM from one base share storage" overstated what is shared | real | `04-runtimes.md` §6 |
| S54 | The `oci` driver is root-equivalent through containerd, and the design did not say so | **real** | `02-architecture.md` §6.1; `17-security.md` §6; `21-stack.md` §6 |
| S55 | "Two binaries" for an agent of several executables in two languages, with no versioning between them | real | `18-operations.md` §2; `02-architecture.md` §6.2; `10-api.md` §11 |
| S67 | Nested virtualization was listed as a node requirement; bare metal needs only KVM | real | `18-operations.md` §2 |
| S77 | The case for Rust at the boundary called a hard kernel error a race, and grouped seccomp with mechanisms it differs from | real | `21-stack.md` §4 |
| S56 | Issue #141 was described in four places as open and unanswered since 2016; it was answered the same day and closed in three | **real, an evidence error** | `research/evidence.md` §1.6; `02-architecture.md` §1; `19-governance.md` §1, §7 |
| S57 | The schema was frozen in Phase 0 while tenancy, the Recipe, and the config schema arrived in Phases 2 and 4, and providers were built before the contract they plug into | **real** | `20-roadmap.md` §2, §3, §4 |
| S58 | "The licence forecloses an open-core pivot structurally" overstated what a licence can do: new proprietary components need no relicensing, and a sole copyright holder can relicense future versions | **real** | `19-governance.md` §2, §9; `LICENSE`; `CONTRIBUTING.md`; `README.md` |
| S59 | "Using it unmodified obliges nothing" missed the obligations attached to conveying copies, the web client's bundle included | real | `00-overview.md` §5; `19-governance.md` §2; `11-surface-web.md` §7; `13-surface-cli.md` §8; `LICENSE` |
| S60 | K-13 to K-19 were presented as derived from evidence and none was; K-19 restated K-8; K-18 leaned on an unsourced market claim | real | `research/evidence.md` §4; `00-overview.md` §6; `04-runtimes.md` §3 |
| S61 | "Six of twelve" advisories: thirteen existed at the date, one CVE was counted twice, one advisory was missing, and one fix version was wrong | real | `research/evidence.md` §1.1; `00-overview.md` §3; `20-roadmap.md` §2 |
| S62 | Five issues cited as current defects had been fixed in 2020 or 2022, or were not defects | real | `research/evidence.md` §1.2, §1.4, §5 |
| S63 | `SECURITY.md` said every issue in the evidence file had been fixed | real | `SECURITY.md` |
| S64 | The README's account of §16 disagreed with §16's own table, sixteen real against seventeen | real | `README.md`; `CONTRIBUTING.md`; `CLAUDE.md`; §16 |
| S66 | Governance required two maintainers with independent rights from the first commit; the project has one | **real** | `19-governance.md` §6, §7, §9; `research/evidence.md` §1.7; `00-overview.md` §5, §6; `21-stack.md` §3 |
| S68 | "About forty" open questions: 117 are recorded and 59 are open | real | `README.md`; `CONTRIBUTING.md`; `20-roadmap.md` §9 |
| S69 | The licence was AGPLv3 in two places and AGPL-3.0-or-later in a third; the documents' CC BY licence appeared only in `LICENSE`, with no text | real | `00-overview.md` §5; `19-governance.md` §2; `README.md`; `LICENSE`; `LICENSE-CC-BY` |
| S70 | The roadmap's graph disagreed with the documents' dependency headers and did not say which kind of dependency it drew | real | `20-roadmap.md` §3 |
| S71 | The evidence file quoted a competitor's prices, which `CLAUDE.md` keeps out of the repository | real | `research/evidence.md` §3 |
| S72 | Findings 7 and 8 still read `OPEN` in their traces, though §15 marked both patched | real | §4, §5 |
| S73 | A table and several references pointed at the wrong thing: the install verb table listed a non-verb and omitted six verbs, and §16 cited wrong sections for R4, R7, and the edge | real | `09-recipes.md` §4; §16 |
| S74 | "These are ordered; the higher number yields" made P10 the principle that yields to all the others | real | `00-overview.md` §3 |
| S75 | `CLAUDE.md`'s removed-material check matched itself; the overview linked the evidence file twice in one sentence | real | `CLAUDE.md`; `00-overview.md` §6 |
| S76 | Eight Wings advisories published after the evidence file's date, four of them in the classes K-1 and K-3 exist for | real, strengthens K-1 and K-3 | `research/evidence.md` §1.1, §1.2 |

### What this review caught, and why the earlier methods could not

Most of the seventy-nine are not new design errors. They are **patch-propagation defects**: a
correction applied to the document that owned it and left standing in the documents that restated
it. R7 moved power out of the intent body in `01-model.md`, and other documents kept starting a
workload by changing its intent (S2). R11 corrected the epoch arithmetic in `03-state.md` and
`18-operations.md` kept the wrong version (S9). R4 removed the subtree walk and the cache
invalidation design brought it back (S10). The governance claims about maintainers, the licence, and
#141 were each stated in one place and repeated, in slightly different words, in up to four.

Neither earlier instrument could see this. A trace follows one story and checks that each step has
an owner; it does not reread every document that mentions the step. An algebra review asks whether
one document's definitions support its claims; a claim that is true in its owner and false in its
copy passes in the owner and is never read in the copy. What finds them is reading everything
against everything, and checking every external claim against its source rather than against the
sentence that cited it. `CLAUDE.md` now requires a correction to reach every document that restates
it, in the same commit.

The evidence errors deserve their own sentence, because the evidence file is the document that holds
every other to account. Nine of its citations were wrong, and all but one (a fix version) erred in
the same direction: toward a stronger case against Pterodactyl than the record supports. None of the
rules built on them fell, and two (K-1, K-3) are better supported now than when they were written,
but a file that demands accuracy of others was the least accurate document in the repository, and
that is recorded here rather than only fixed there.

### What was not accepted

Nothing. That is not because the review was infallible: every finding was checked against the
files and the primary sources before it was recorded, so claims that did not survive were never
entered, and this table cannot show what the review got wrong the way §16's could. That is a real
loss of information, and the next review should record its rejected claims as §16 did.

---

## 18. A pipeline deletes a workload

Written 2026-10-13, from the implementation, when `DeleteWorkload` was built.

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | `mc-testing` is removed from the set's files; apply proposes its deletion, never performs it | `13-surface-cli.md` §5 |
| 2 | The deletion is a retire plan computed from the approved target and power: stop, detach and retain volume `world`, remove the runtime | `01-model.md` §3.3; `02-architecture.md` §4.8 |
| 3 | It waits for a human: removing a runtime is disruptive by the vocabulary's floor, and `workload.delete` is irreversible | `01-model.md` §3.3; `10-api.md` §5 |
| 4 | Approved: every other proposal is superseded, and a moderator's stop is refused, the workload is being retired | `01-model.md` §3.3 |
| 5 | The node's assignment carries `retire`; the agent stops the workload and removes the runtime, touching no volume | `02-architecture.md` §4.8 |
| 6 | The completion effect of the last step is ingested, checked against the node's own steps of that plan: the plan is applied, the assignment leaves the set, the placement is removed, `deleted_at` is set, and the name and quota are released ‡ | `02-architecture.md` §4.8, §4.10; `05-scheduling.md` §5.1 |
| 7 | Volume `world` remains, detached in step 6's transaction, listed and never collected ¶ | `06-storage.md` §7; `01-model.md` §3.6 |
| 8 | Its console stream remains too: the shim closed the task's pipes at step 5, the writer sealed the last segment, and the agent journaled it, believed from the node that held the lease though the assignment has left the set § | `14-streams.md` §4; `01-model.md` §3.6 |

**FINDING 24: A deleted workload's identity has no moment it is deleted. `CONFIRMED`**

`deleted_at` was "soft; the id is retained forever", and every reader of it (name uniqueness,
quota admission, listing) depended on when it was set, which nothing said. Set at the request, a
name would be reusable and quota released while the old runtime still held its node's capacity;
set at approval, the same, and a cancelled retirement would have to un-delete an identity whose
name someone may have taken. A workload nothing was ever approved for is the other edge: no node
holds it, so no node will ever report its retirement complete, and waiting for one would keep it
forever.

**Resolution.** A retire plan moves no pointer. Approving it freezes the workload, and `deleted_at`
is set when the retirement is complete: when the node's completion effect is ingested, or at
approval when the plan has only the control plane's step because nothing ever reached a node.
Cancelling a retirement before any step has run restores nothing, because nothing moved. → patched
into `01-model.md` §3.3 and `02-architecture.md` §4.8

‡ Updated 2026-10-13, when journal ingestion was built. Step 6 trusted whatever the node reported.
A completion is now believed only for a step that is the node's own in that workload's plan, and a
record that is not is kept as a rejection; and a retire plan approved while the create plan was
still applying waited in `queued` with nothing to move it on, so a queued plan now becomes the
node's next plan once the one ahead of it is applied (§4.8 of `02-architecture.md`).

§ Added 2026-10-14, from the implementation, when console capture was built.

**FINDING 28: The last segment of a retired workload's console was refused. `CONFIRMED`**

The control plane believed a segment record only from a node the workload was placed on or
assigned to. A console's last segment is sealed when its container goes, which is step 5, and it
reaches the journal after step 6 has taken the assignment out of the set, so it was refused as a
stranger's, and a retired workload's final output, the one thing §3.6 of `01-model.md` promises a
stream keeps, was missing from the index. A prune is worse: it happens days after the workload
left, on the node's own retention. The end-to-end test found it; no document had asked what
evaluates the check, and with what information, after the assignment is gone.

**Resolution.** A segment record is believed from any node that has held the workload's lease,
released or not: leases are never deleted, so the control plane holds that history, and a node
writes a stream only while it runs the workload, which takes a lease. Retiring keeps the stream as
it keeps volumes, and deleting it is the volume deletion step. → patched into `14-streams.md` §4

¶ Added 2026-10-14, from the implementation, when volume objects were built. Had the deletion
named `world`, step 2 would retain nothing and add, after `retire`, an irreversible
`delete_volume` step carrying the helper's identifier the node reported; step 7 would then be the
node's deletion record, which marks the volume deleted rather than removing it (§7 of
`06-storage.md`). Kept, as here, `world` is deleted later by `korpis volume delete`: a plan of
kind `delete_volume` for `mc-testing`, retired, approved by a person, and carried to `fra-3` by a
residual assignment that leases nothing and leaves the set once the plan is applied (§4.8 of
`02-architecture.md`). Or a new workload attaches it with `existing`: declared against it, it is
claimed when that workload's target is approved, placed on `fra-3` because its disk is there, and
mounted from the same directory (§7 of `06-storage.md`).

**FINDING 30: A retained volume was listed by nothing. `CONFIRMED`**

Step 7 says the volume is "listed and never collected", and §7 of `06-storage.md` makes the listing
the whole defence against losing an orphan: nothing collects it because someone can see it. Asked
what evaluates "listed", and with what information, the answer was nothing. The control plane held
no volume object, only the intent that declared one, and an intent says what was asked for, not
what a node made, where, or whether the size it carries is enforced; the volume's only record was
a directory on the node. A volume made for a workload that was then retired was invisible to every
client, which is the orphan §7 exists to show.

**Resolution.** The node journals every volume it ensures or deletes, after the helper's answer,
and the control plane keeps the object from those records, believed from a node that once held
the workload's lease (the test of Finding 28, for the same reason: deletion comes long after the
workload left). Retiring the workload detaches its volumes in the transaction that sets
`deleted_at`, so step 7 holds the moment step 6 does, and `VolumeService` with `korpis get volumes`
lists them, a size shown only where the class enforces it. → patched into `06-storage.md` §7 and
`01-model.md` §3.6

---

## 19. A griefed world is rolled back

Written 2026-10-14, from the implementation, when snapshots were built. A moderator of
`mc-survival`, whose volume `world` is on `fra-3` in a btrfs class, takes a snapshot before a
plugin update; a griefer then burns half the map, and the moderator rolls back.

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | `korpis volume snapshot vol_… before-update`: the control plane reads the class of `world` on `fra-3` from what the node's `Hello` declared; it declares `snapshot`, so a plan of kind `snapshot_volume` is proposed under `volume.snapshot.create` on the volume | `06-storage.md` §2, §4; `10-api.md` §5 |
| 2 | Its one step changes nothing the workload sees, so it applies at once, and it is a plan of `mc-survival`, which holds the volume | `01-model.md` §3.3; `06-storage.md` §7 |
| 3 | `mc-survival` runs on `fra-3`, so its own assignment carries the step; the helper takes a read-only snapshot under the class's `.snapshots/`, and the node journals it after the helper answers | `02-architecture.md` §4.8, §4.10; `06-storage.md` §4 |
| 4 | The control plane keeps the snapshot from that report, believed from a node that held the workload's lease, and lists it on the volume | `01-model.md` §3.6; `06-storage.md` §4 |
| 5 | `korpis volume rollback snp_…` proposes, under `volume.snapshot.rollback`, an irreversible plan: stop, roll back, start ‡ | `10-api.md` §5; `06-storage.md` §4 |
| 6 | It waits for a person; once approved it is carried like any plan, its steps executed in order under the lease | `01-model.md` §3.3; `02-architecture.md` §4.8 |
| 7 | The helper makes a writable copy of the snapshot, exchanges it with the volume in one rename, deletes the old data, and sets the volume's byte limit on the new subvolume; started again, the workload runs on the map as it was | `06-storage.md` §3, §4 |
| 8 | Deleting the snapshot later is its own irreversible plan under `volume.snapshot.delete`; deleting the volume would have taken its snapshots with it | `06-storage.md` §4, §7 |

**FINDING 31: A rollback under a running workload replaces what it has open. `CONFIRMED`**

§4 of `06-storage.md` called a snapshot "a cheap local rollback point" and said nothing of what
the workload is doing while it is rolled back to. Asked what evaluates the rollback, and where,
the answer was the helper, swapping the volume's directory on the node, under a container that
has that directory mounted and files in it open. The running server keeps writing to the old
data through what it holds, and the new data is what it finds when it next opens a file: two
worlds, neither the snapshot.

**Resolution.** A rollback of a volume a running workload holds is a plan of three steps, stop,
roll back, start, so the swap happens with nothing holding the volume, and starting builds the
workload's mounts again from the volume's path, which is now the snapshot's copy. A volume no
workload runs on is rolled back alone. → patched into `06-storage.md` §4 and §7

‡ Step 5 is where the trace found Finding 31.

## 20. A world is backed up every night

Written 2026-10-14, from the implementation, when backups were built. The operator keeps
`mc-survival`'s backups in an S3 bucket behind the organization's backup target provider `s3`,
and wants one every night at 03:00 without anyone typing a command.

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | `korpis repository create nightly --provider s3 --prefix korpis/` under `repository.write`; the key-encryption key is made and sealed by the control plane's backup key. A second repository, `offsite`, naming `s3` at `korpis/` again is refused ‡ | `06-storage.md` §5.3, §5.5; `10-api.md` §5 |
| 2 | The nightly backup is a `scheduled` workload whose run, at 03:00 with `concurrency: forbid`, calls `korpis backup create vol_… --repository rep_…` with its own workload credential, holding `backup.create` on `mc-survival` and `repository.read` on `nightly` † | `01-model.md` §2; `05-scheduling.md` §4; `08-identity.md` §9.5 |
| 3 | The control plane finds the volume live, the repository serving the workload's project, and no backup of the workload into it open; it names the last whole backup the parent and reads from what `fra-3` declared whether the class can snapshot. A plan of one `backup_volume` step changes nothing the workload sees, so it applies at once, and the backup is recorded pending | `06-storage.md` §5.1, §5.5; `01-model.md` §3.3 |
| 4 | `mc-survival`'s assignment carries the step to `fra-3`. The agent asks the helper for job `bkp-<backup>`; the helper knows none, so the agent asks the control plane for the job's key and access, naming the plan and the step | `02-architecture.md` §4.8, §4.9; `06-storage.md` §5.5 |
| 5 | The control plane answers because `fra-3` holds the volume, the plan is open, and the backup is pending: it makes the workload's data key at its first backup there, opens it, and asks `s3` for twelve hours of write access to the workload's prefix alone | `06-storage.md` §5.5, §5.6 |
| 6 | The helper snapshots the volume for the job and starts it confined to its cgroup, to the snapshot and the store's port by Landlock, with no capabilities; the agent's loop goes on serving the node's other workloads while it runs | `06-storage.md` §5.5; `17-security.md` §6; `02-architecture.md` §4.9 |
| 7 | When the job has ended, the agent journals its result and has the helper forget it, which releases the snapshot; the control plane believes the result only from the node holding the volume, marks the backup whole, and then the plan applied | `02-architecture.md` §4.10; `06-storage.md` §5.1 |
| 8 | `korpis backup list mc-survival` shows it under `workload.read`; the parent organization sees that it exists and what it stores, and neither its file tree nor its contents | `10-api.md` §5; `08-identity.md` §3.3, §7 |
| 9 | The next night's backup names this one its parent, and the job reads again only the files whose size, times, or inode changed | `06-storage.md` §5.7 |

**FINDING 32: Two repositories at one place would prune each other. `CONFIRMED`**

§5.3 of `06-storage.md` let a repository name any provider and any prefix, and §5.5 there rests
"one writer per workload prefix" on the control plane scheduling every job that touches a prefix.
Asked what evaluates that, the answer was a check on open backups per workload *and repository*,
which is one writer per prefix only if two repositories never share one. Nothing said so: the same
workload backed up into `nightly` and `offsite` at the same place writes one prefix under two
keys, and each repository's prune, finding objects none of its manifests reference, deletes the
other's backups.

**Resolution.** Within an organization, where a provider name resolves to one installation, a
repository is refused at the same prefix as another of the same provider, or at one beginning with
the other's followed by `workloads/`. Across organizations nothing is checked, because a check
would tell one tenant where another keeps its backups, and none is needed: a workload is only
backed up into its own organization's repositories, beneath its own identifier, and a prune lists
one workload's prefix at a time, never a repository's whole prefix. Two provider names the
operator pointed at one bucket stay invisible to the control plane, and §5.3 says so. → patched
into `06-storage.md` §5.3 and §5.5

**FINDING 33: A nightly backup had nothing to run it. `CONFIRMED`**

§2 of `01-model.md` gives "nightly backup" as the example of a `scheduled` workload, §4 of
`05-scheduling.md` defines its runs and their policy, and §7 of `22-first-party.md` answers
scheduled restarts and wipes with the same. No phase of `20-roadmap.md` builds either lifecycle,
so a backup existed only when someone typed it, and Phase 1's "back them up" would have been met
by a command nobody runs at 03:00.

**Resolution.** `task` and `scheduled` workloads, their runs, and `RunPolicy` are Phase 1 work,
built after backups so that a scheduled run is the first thing to take one. What evaluates the
schedule is the control plane, which holds the intent's policy and the time; what proposes the
backup is the run, under its own grants, so a nightly backup is authority someone granted rather
than a feature of the backup subsystem. → patched into `20-roadmap.md` Phase 1

‡ Step 1 is where the trace found Finding 32. † Step 2 is where it found Finding 33.

## 21. Last night's world, over tonight's

Written 2026-10-14, from the implementation, when restores were built. The morning after §20's
backup, a griefer has burnt `mc-survival`'s spawn. The moderator first pulls last night's
`world/` beside the live one to compare, then replaces the whole volume with last night's backup.

**Trace**

| # | Step | Governed by |
|---|---|---|
| 1 | `korpis backup list mc-survival` shows last night's backup, whole, under `workload.read` | `06-storage.md` §5.1; `10-api.md` §5 |
| 2 | `korpis backup restore bkp_… --path world --into last-night`: a plan of one `restore_volume` step, under `backup.restore` on the workload, loses nothing and applies at once | `06-storage.md` §5.4; `01-model.md` §3.3 |
| 3 | The node holding the volume asks for the step's key and access; the control plane answers it with the backup's own workload key and read access to that workload's prefix, because the backup is whole and the node holds the target volume † | `06-storage.md` §5.4, §5.5 |
| 4 | The helper makes `last-night` at the volume's root, owned as the root is, and the job restores `world/` into it and verifies what it wrote; the workload never stopped | `06-storage.md` §5.4; `04-runtimes.md` §4.5 |
| 5 | `korpis backup restore bkp_… --replace` proposes `restore_volume`, `stop`, `replace_volume`, `start`; the replacing step is irreversible, so the plan waits for the moderator's approval | `06-storage.md` §5.4; `10-api.md` §5 |
| 6 | Approved, the job restores into a staging place beside the volume while players are still on; nothing counts it against the volume's quota ‡ | `06-storage.md` §5.4 |
| 7 | The server stops; the helper exchanges the staging place with the volume in one rename, removes the old data, and keeps the volume's limit; the server starts on last night's world | `06-storage.md` §5.4; `02-architecture.md` §4.9 |
| 8 | The plan leaves the set, and the agent forgets the jobs it no longer names | `02-architecture.md` §4.9 |

**FINDING 34: Replacing a volume needs room for two copies of it. `CONFIRMED`**

§5.4 of `06-storage.md` offered a full restore without saying where it is written. Asked what
holds the restored data until the exchange, the answer is the node's filesystem, beside the live
volume, in full: a 40 GB world needs 40 GB free that no quota, placement, or plan reserved. The
staging place carries the volume's limit, so the restore cannot outgrow it, but the filesystem
can be full first.

**Resolution.** Stated rather than reserved: the job fails with the filesystem's error, the
volume is untouched because nothing is exchanged before the job verified, and the staging place
is discarded with the plan. A restore into a directory needs the same room inside the volume's
own limit. Reserving the room would mean placement accounting for restores it cannot foresee,
and the failure it would prevent loses nothing. → patched into `06-storage.md` §5.4

**FINDING 35: A prune could remove the backup a restore is reading. `CONFIRMED`**

§5.5 of `06-storage.md` keeps a prune off a prefix while a backup writes to it, by one lock per
prefix. A restore is a reader: it holds read access to the same prefix for up to twelve hours and
takes no lock, and nothing kept a prune scheduled in that window from deleting the backup's
record or sweeping the chunks it was about to fetch. The restore would fail halfway, and in
step 7 the moderator would have stopped the server for nothing.

**Resolution.** A backup that an open plan restores is not pruned, and nothing it references is
swept, until that plan ends. What evaluates it is the control plane, which holds both the open
plans and the prune. → patched into `06-storage.md` §5.5; the prune task carries it

‡ Step 6 is where the trace found Finding 34. † Step 3 is where it found Finding 35.
