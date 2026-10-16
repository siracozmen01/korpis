# The CLI and Declarative Workflow

**Status:** design **Date:** 2026-08-07 **Depends on:** [`10-api.md`](./10-api.md),
[`03-state.md`](./03-state.md), [`08-identity.md`](./08-identity.md) **Implements:** Principles P2,
P5, P6

---

## 1. The CLI is the reference client

`korpis` is one binary, calls only the published API, and is the client against which P2 is
mechanically enforced: **every API operation has a command, and no command has a private
endpoint.** Coverage is checked in CI against the schema, so "the API can do something no client
exposes" and "a client does something the API cannot" are both build failures rather than opinions.

The check is a command that reaches every method by name: `korpis call WorkloadService/GetWorkload
-d '{"id": "wl_…"}'` sends the request as the API's JSON and prints the answer the same way, and a
server stream prints each message as it arrives. Its table of methods is written out by hand, and a
test compares it with the schema's own list of `korpis.v1` methods, so a method added to the API
without a command fails the build. The friendlier commands are built over the same calls, never
beside them. A mutating request is given a fresh idempotency key when it carries none, so a retry
the CLI makes is safe and two invocations are two requests (§6 of `10-api.md`).

The daemons (control plane and node agent) are separate binaries with their own operational
surface, covered in `18-operations.md`. This document is about the client.

---

## 2. Two ways to say the same thing

```
korpis workload restart mc-survival        imperative: one object, right now
korpis apply -f servers/                   declarative: a set of objects, converge
```

Both produce an `Intent`. Both produce a `Plan`. Both appear in the same audit trail. The
imperative form is a convenience over the declarative one, not a second path through the system,
which is why the two cannot drift apart the way `kubectl edit` and a GitOps controller do.

Human output by default; `--json` emits **the API's own object shape**, not a CLI-specific
rendering. Nothing exists in the JSON that a program calling the API directly would not receive, so
a script that outgrows the CLI does not have to be rewritten to move to the SDK. Exit codes are
meaningful and map to the error codes of §8 of `10-api.md`, `DENIED`, `CONFLICT`, and
`UNSATISFIABLE` are distinguishable without parsing text:

| Exit | Meaning |
|---|---|
| 0 | done |
| 1 | refused by the CLI itself before anything was sent: a file, an argument, a value |
| 2 | the command line does not parse |
| 3 | the control plane could not be reached, or answered without a structured error |
| 10 to 19 | `INVALID`, `UNAUTHENTICATED`, `DENIED`, `NOT_FOUND`, `CONFLICT`, `UNSATISFIABLE`, `QUOTA_EXCEEDED`, `OVERFLOW`, `UNAVAILABLE`, `INTERNAL`, in that order |

With `--json` a failure is written to standard error as the API's error (code, reason, field, and
message), so a script reads the same object whether it called the API or the CLI.

The imperative commands are `get` (organizations, projects, workloads, nodes, plans, effects),
`status`, `plan approve|reject|cancel`, `workload
start|stop|restart|place|classify|delete`, and `node token create|cordon|uncordon`, each one
call of the API once its names are resolved. An object is named
by its identifier, by its name, or, for a workload, as `project/name`. A name is resolved by
listing what the caller may read, never by a lookup the API does not offer, so a name the caller
cannot read is the same local refusal (exit 1) as a name that does not exist, and the CLI learns
nothing by resolving that the API would not have told it (§8 of `10-api.md`). Listing starts from
the organizations the caller may read and from the objects its own grants are anchored at, so a
token scoped to one project names that project though it can read no organization to list it
from. A name that matches
more than one object is refused with the advice to use the identifier, not resolved to the first.
The cost is calls: a bare workload name lists every project the caller can read, which a caller
who can read many pays for on each command; `project/name` or the identifier avoids it. A power
change is made against the power version the CLI has just read, so two operators who stop and
start the same workload at once see one of them get `CONFLICT` rather than both silently winning.

---

## 3. Declared files are intent bodies

A declared file is the intent body. There is no wrapper, no `apiVersion`, no `kind` repeated in
every document, no metadata block that exists to satisfy a machine:

```yaml
# servers/mc-survival.yaml
workload: mc-survival
project: community-servers
recipe:  ghcr.io/korpis/recipes/minecraft-paper@sha256:9d2f…
lifecycle: persistent
console: interactive
resources:
  memory: { reservation: 4Gi, limit: 6Gi }
  cpu:    { reservation: 2 }
volumes:
  - name: world
    size: 40Gi
    class: replicated
endpoints:
  - name: game
    port: 25565
    exposure: stable
config:
  motd: "Survival · season 4"
  difficulty: hard
```

The file is validated against the recipe's config schema (§5 of `09-recipes.md`) **before**
anything is sent, so a typo in a field name is a local error with a line number rather than a
rejected request.

What the file leaves out (the runtime, the probes, the volume's mount path above) is supplied by
the recipe's preset, on the control plane, at declaration (§3.1 of `09-recipes.md`), and the plan
shows the completed body.

The keys are the ones above and a few more: `power` (`running` or `stopped`) and `labels` for a
new workload; `pids` among the resources; `path`, `mode`, `existing`, and `from_backup` on a
volume; `protocol`
and `domains` on an endpoint. Quantities are written as `4Gi` or `500m`, and enum values as the
API names them, lower-case and without their prefix (`node_port`, `overlay_only`). The recipe is
a repository with `@sha256:…` or `:tag`; the project is a name, looked up among the projects the
caller may read and required to be unique there, or a `prj_` identifier. `-f` takes a file or a
directory, whose `.yaml` and `.yml` files are read in name order. A key the format does not know is
refused, never ignored.

The check before sending is the control plane's own code run locally: the recipe is fetched by
digest, the declaration completed from it, and the configuration resolved against its schema, so
`config.greting` is refused as `servers/mc.yaml:5: config.greting: not declared by the recipe`.
What only the control plane can decide (authority, quota, placement) it decides when the file
arrives.

---

## 4. Plan is the diff, and it is the real one

```
korpis plan -f servers/
```

This is the place the model pays for itself. `kubectl diff` computes a difference on the client and
shows you a text comparison of two YAML documents; what the cluster will actually do remains a
guess. Here, `plan` sends the declaration and receives the **real, persisted `Plan` object**, the
same object the web panel would show, computed by the same code that will execute it, including
placement decisions, `Explanation`s, quota effects, and estimated unavailability.

```
Plan pl_3c81 · 3 changes

  ~ mc-survival        memory 4Gi → 6Gi          restart required, ~40s, 14 players connected
  + mc-creative        create on node fra-3      fra-3: most free memory after packing
                                                 (see: korpis explain pl_3c81 mc-creative)
  - mc-testing         delete                    volume "world" 12Gi will be RETAINED
                                         (pass --delete-volumes mc-testing/world to remove)

quota  community-servers   memory 18Gi → 24Gi of 32Gi

apply with:  korpis apply -f servers/ --plan pl_3c81
```

Applying a Plan by identifier applies **that** Plan. If the world moved in between, apply returns
`CONFLICT` (§6 of `10-api.md`) and shows the new Plan, rather than silently executing something the
operator never read. A plan reviewed in a pull request is therefore the plan that runs.

> Note, 2026-10-13, from the implementation (`crates/korpis-cli`). The API's `dry_run` persists
> nothing, so a plan computed by it has an identifier nothing can apply, and this section's
> `--plan` had nothing to name. `DeclareIntent` gained `propose`: the intent and its plan are
> persisted and the plan waits for confirmation even when default policy would approve it at once
> (§3.3 of `01-model.md`). `korpis plan` declares with it; `korpis apply --plan` confirms the plan
> after declaring the same files as a dry run and comparing the completed body with the one the
> plan was computed for, and a difference is the `CONFLICT` above. Plans are per workload, as
> the model has them, so a directory of files gives one plan each, and `--plan` is given once per
> workload. Planning again supersedes the plan before it, so only the newest plan for a workload
> can be applied. Without `--plan`, `apply` declares and default policy approves what it may.

---

## 5. Pruning without the footgun

The unsolved problem in every declarative tool: what happens to an object that was removed from the
files. Guess wrong and you either accumulate orphans forever or delete a customer's world directory
because someone renamed a file.

**A declared set has an owner, and removal from the set is a proposal, never an act.**

```
korpis apply -f servers/ --set community-servers
```

Objects created through a set are marked with it. An object that was in the set and is no longer in
the files appears in the Plan as a deletion, enumerated by name, never summarized as "3 objects
pruned", and:

- Deleting a workload is permitted by an ordinary apply.
- **Deleting anything that holds data is not.** Volumes, snapshots, and backups are retained by
  default and require `--delete-volumes` naming them explicitly. §5.3 of `06-storage.md` gives
  backups a `min_retention` floor in their retention policy, and the CLI cannot override it.
- An object that never belonged to the set is never touched by it, so two sets cannot fight and a
  hand-created workload cannot be pruned by someone else's pipeline.

How the CLI carries this out (the set itself is §3.3 of `01-model.md`). The set is named within
each project the files declare into, and the first apply through it creates it. Before anything
is declared, every file is declared as a dry run through the set, so drift (§6) fails the apply
before the first change rather than after half of them; `--adopt` takes into the set a workload
the files declare that belongs to no set, and without it such a workload is a refusal, not a
silent claim. A deletion is a retire plan, which always waits for a person (§3.3 of
`01-model.md`): `korpis plan --set` proposes it and lists it with the other plans, `korpis apply
--set` proposes and confirms it in the same run, and `apply --plan` deletes exactly the members
whose retire plans it names. `--delete-volumes` names each volume as `member/volume`, and a name
for a member the apply does not delete is refused rather than ignored, because deleting data is
never inferred from a name that happens to match; outside a set, `korpis workload delete
--delete-volumes world,stream_console` names the workload's own (`stream_console` is its console
stream's volume, §4 of `14-streams.md`). Each named volume becomes its own irreversible step after
the runtime is removed, under `volume.delete` on the volume, and the plan view marks those steps
`IRREVERSIBLE` (§7 of `06-storage.md`). A volume kept by an earlier deletion, whose workload is
gone, is deleted with `korpis volume delete vol_…`, which proposes a plan of that one step and
leaves it waiting for a person, like every deletion of data. `korpis volume snapshot vol_… <name>`
takes a snapshot where the volume's class can, `korpis volume snapshots vol_…` lists them, and
`korpis volume rollback snp_…` and `korpis volume delete-snapshot snp_…` propose the two that
lose data, each waiting for a person (§4 of `06-storage.md`).

Backups are kept in repositories. `korpis repository create nightly --provider s3 --prefix
korpis/` makes one for the organization, or for one project with `--project`, keeping by default
the last backup of each of seven days, four weeks, and six months above a one-day floor;
`korpis repository list` and `korpis repository get rep_…` read them (§5.3 of `06-storage.md`).
`--kms-provider <name> --kms-key <key>` makes it under tenant custody (§5.5 of `06-storage.md`):
its data keys are made and opened by the tenant's own key management service, through that
provider, and never pass through the control plane in the clear.
The first repository of an organization or project also takes its workloads' console streams
(§4 of `14-streams.md`) unless made with `--no-streams`; `--streams` asks for it explicitly.
`korpis backup create vol_… --repository rep_…` backs a volume up, a plan that applies at once;
`korpis backup list <workload>` shows its backups newest first, read from a snapshot or live, and
`korpis backup get bkp_…` one, with why it failed and what it did not read (§5.1 there).
`korpis backup restore bkp_… --into <dir> [--path <p>]` restores it into a new directory of the
volume at once; `--replace` replaces the whole volume, a plan that waits for approval and stops
the workload only for the exchange; `--volume vol_…` restores into another volume (§5.4 there).
`korpis backup ls bkp_… [dir]` lists one directory of a backup without a byte of any file, and
`korpis backup cat bkp_… <file> [-o out]` writes one file, each under its own action.
`korpis backup clone bkp_… --name <new>` declares a workload like the one the backup is of, its
backed-up volume created holding the backup and every other volume created empty, which is how a
restore is tested. `korpis backup prune <workload> --repository rep_…` proposes forgetting what the
repository's retention policy no longer keeps, listing each backup it forgets and why every other
one stays; it waits for approval, and what only the forgotten backups held is deleted after a
grace period (§5.5 there).

> Corrected 2026-10-14, when volume deletion was built. This paragraph said `--delete-volumes`
> was refused in Phase 0 and kept a deleted workload's volumes, the safe half of the rule. The
> other half is built now; the flag's bare names became `member/volume`, because a set deletes
> several members at once and one bare name could match a volume of each.

`korpis set list` and `korpis set mode` read a project's sets and change one's mode.

The names an organization's endpoints may route are its domains (§3.2 of `07-networking.md`).
`korpis domain create shop.example.com` asks for one and prints the TXT record to publish, and
`korpis domain verify dom_…` has the control plane look for it; `--delegated-from dom_…` makes one
beneath an ancestor organization's verified domain, and `--attest` vouches for one as an operator,
each verified at once. `korpis domain list` and `korpis domain get dom_…` read them, and
`korpis domain delete dom_…` deletes one with every domain delegated beneath it.

A certificate an organization brings for its endpoints is uploaded with `korpis certificate create
--chain chain.pem --key key.pem`, which shows the names the chain's leaf is for and its expiry; an
endpoint serves with it by naming its `crt_…` identifier in `tls.secret`. `korpis certificate
replace crt_… --chain … --key …` renews it in place, so the endpoints serving with it need not be
declared again, and is refused if the new chain no longer covers a name one of them routes.
`korpis certificate list` and `korpis certificate get crt_…` read them, the chain but never the
key, and `korpis certificate delete crt_…` deletes one no endpoint serves with.

---

## 6. Two sources of truth, reconciled honestly

Files declare intent. So does the web panel, and so does Discord. Pretending otherwise produces the
GitOps failure everyone has lived through: someone fixes an outage through the UI at 03:00 and a
controller silently reverts it four minutes later.

Every `Intent` carries `managed_by`. When an intent owned by a declared set is changed out of band,
the behaviour is declared per set:

| Mode | Behaviour |
|---|---|
| `advisory` (default) | the out-of-band change stands; the set is marked **drifted**, the drift is shown in the panel and in `korpis status`, and the next `plan` shows it as a change to be reconciled or adopted |
| `strict` | the next apply reconciles it back, and the override that was discarded is recorded as an `Effect` with its author, so it is recoverable and attributable |

Neither mode ever silently loses a human's emergency fix. `advisory` is the default because the
03:00 fix is usually right, and a system that punishes people for fixing outages gets worked around
within a month.

The set is a `DeclaredSet` of §3.3 of `01-model.md`, named within a project and created by the
first apply through it; `managed_by` names it on every intent its applies declare, and drift is
derived from that pointer rather than stored. The control plane, not the CLI, decides what a
declaration through the set does to a drifted member, because the panel and any other client can
declare through a set too: a body equal to the drifted head is the files catching up with the fix
and is accepted in either mode; anything else is declared over the fix by a `strict` set, refused
with `CONFLICT` reason `set.drifted` by an `advisory` one unless the declaration says `reconcile`,
and the discarded intent is recorded with its author whenever one is declared over.

### What a pipeline does when it arrives at drift

> Finding 18 of `23-walkthroughs.md`.

"Reconciled or adopted" describes a person at a terminal. §7 calls CI the good case for a scoped
token, and a CI runner can do neither: adopting means editing the repository, which a pipeline
should not do to itself, and reconciling means discarding the overnight fix, which is the thing
`advisory` exists to prevent.

Three behaviours are defensible and picking none means implementations pick differently and
operators find out by being surprised. So:

**An `advisory` set with drift fails the apply and applies nothing.** The failure names each
drifted object and the `Effect` that caused the drift, with its author and timestamp, and exits
with `CONFLICT` (§2). `--accept-drift` proceeds while leaving the drifted objects untouched, for
the pipeline whose operator has decided the disagreement is known and tolerated; `--reconcile`
declares over them, and each discarded change is recorded with its author. `korpis status` shows
a drifted member's author and effect and every quota-bearing figure on which the drift and the
set's files disagree (finding 17 of `23-walkthroughs.md`).

Failing the whole apply rather than skipping the drifted objects is deliberate: a partial apply
against a set someone is actively editing by hand is how two sources of truth become three.

A pipeline going red at 09:00 because a human fixed something at 03:00 is the correct outcome. The
fix survives, nothing is reverted by a machine, and a disagreement between the file and reality is
escalated to a person instead of being settled by whichever wrote last.

`strict` is unchanged: it reconciles, and the discarded override is recorded as an `Effect` with
its author, which is what makes choosing `strict` an informed decision rather than a lossy one.

---

## 7. Authority, and why CI is the good case

No long-lived admin key in a dotfile. Interactive login is a browser or device-code flow that
yields a short-lived capability token held in the OS keyring.

Until that flow exists, the CLI keeps no credential at all: the token is read from `KORPIS_TOKEN`
or from a file named with `--token-file` or `KORPIS_TOKEN_FILE`, the control plane from
`KORPIS_SERVER` or `--server`, and its certificate authority from `KORPIS_CA` or `--ca`, a PEM
file trusted alone for it, so no public authority can vouch for a Korpis control plane. Where the
token comes from is the operator's decision and the operator's storage; the CLI writes nothing
down.

For automation, §3 of `08-identity.md` does something RBAC systems cannot:

```
korpis grant issue \
  --actions workload.read,workload.write \
  --scope   project:community-servers \
  --expires 90d \
  --label   "github actions · korpis-community/servers"
```

That token can deploy one project, expires on a date, appears in every `Effect` it authorizes, and,
because grants only ever produce weaker children (P6), **cannot be used to mint a broader one.** A
leaked CI secret in this market is normally an admin key; here it is a bounded, dated, attributable
delegation, and revoking it is revoking one row.

The command issues the grant to a token created to hold it alone, attenuated from the one grant
the issuer holds that it is valid from, and prints the token's secret once (§3.7 of
`08-identity.md`). It is checked online on every request, so the revocation takes effect at the
pipeline's next call. `--to` issues to an existing subject instead; `grant list`, `grant show`
(the grant and its chain), and `grant revoke` complete the set.

> Corrected 2026-10-14 (Finding 45 of `23-walkthroughs.md`). This said the token is not a
> link-form grant because a link is verified offline and stays good until it expires. A link is
> accepted by the gateway alone and revoked at its next call (§6.1 of `08-identity.md`); it is
> not this token because every call spends one of a use cap it cannot go without.

`korpis grant share --actions workload.restart --scope workload:mc-survival` makes a share link
(§6.1 of `08-identity.md`) and prints it once, as a URL whose fragment is the link, with what it
permits, on what, how many times, for how long, and that using it needs the control plane.
`--expires` defaults to `24h` and `--uses` to `3`; neither can be removed. Whoever receives it
uses it as the credential: `KORPIS_TOKEN` takes the link or the whole URL, as it was pasted.

Installing an extension is issuing grants, so it is two steps, as `plan` and `apply` are. `korpis
extension install ARTIFACT --version 1.0.0` changes nothing: it prints what the extension needs,
over what, and why, what it reaches, provides, and subscribes to, and then the command that
approves exactly that, with the artifact pinned by digest. `--approve` installs, and is refused
unless `--version` is a digest, because a tag may move between reading the needs and approving
them, and an approval is of what was read (§4 of `16-extensions.md`). `extension list`, `extension
show`, and `extension uninstall` complete the set, with `extension upgrade ext_… --version 2.0.0`,
which is two steps the same way: it shows the new needs and what would be kept, issued, and
revoked, then `--approve` with the digest upgrades in place (§9 question 6 of `16-extensions.md`);
a grant's holder may be written `ext_…` to name an installation. `extension providers` lists the
provider streams registered in an organization, and `extension ping dns cloudflare` calls one's
liveness check as the organization sees it and prints how the call ended: the registration and
version that answered, or the failure's reason (`provider.deadline`, `provider.stream_ended`,
`provider.unknown`), each under `extension.read` over the organization (§3.3 of
`16-extensions.md`).

Shell completion resolves against what the caller can actually read, for the same reason Discord's
autocomplete does (§8 of `12-surface-discord.md`): existence follows read authority, so completion
cannot become a name-disclosure oracle.

`korpis completion bash|zsh|fish|elvish|powershell` prints a script that registers the binary
itself as the completer, meant to be sourced at shell start (`source <(korpis completion bash)`)
rather than saved, because the protocol between the script and the binary is the binary's own.
On each completion the shell runs the binary, which lists through the API exactly as name
resolution does (§2): workload, project, organization, and node names, plan and grant
identifiers, and `--scope` as `kind:name`. Nothing is cached, so a revoked grant completes nothing
on the next key. The costs: every completion is a process and a few API calls, a pause a person
can notice on a slow link; and the completer sees the environment (`KORPIS_SERVER`, `KORPIS_CA`,
`KORPIS_TOKEN`, `KORPIS_TOKEN_FILE`), not flags typed earlier on the same line. A failure of any
kind completes nothing and prints nothing, because an error message in the middle of a command
line is worse than no suggestion.

---

## 8. The interactive commands

```
korpis console mc-survival           attach to the durable stream; scrollback, seek, replay
korpis logs mc-survival --since 1h   read without attaching
korpis exec mc-survival -- /bin/sh   a session, subject to the runtime's isolation tier
korpis files sync ./world w:mc-survival/world   delta transfer, through a relay
korpis forward mc-survival:25565     local port through the overlay, no public exposure
korpis watch -f servers/             stream plans and effects as they occur
korpis event follow sub_… --ack      print a subscription's events as they come, and record them
korpis version                       client and server versions, and the source revision of each
```

**`logs` as built** (Phase 1): `korpis logs mc-survival [--follow] [--from N | --since 1h]
[--grep P [--regex]] [--limit N] [--timestamps] [--relay URL]` mints a token for
`workload.console.read` on the one workload, with the route the gateway seals with it, and opens a
session at the control plane's relay role, or the first relay it names, or the one named (§3.1 of
`02-architecture.md`). Records arrive in offset order from the node's own files, searched there
when `--grep` is given; dropped records print as the gap line §3 of `14-streams.md` shows, and
offsets the node no longer holds print as such. Following, the command renews its token on the
session thirty seconds before it expires, and a lost session is opened again, with a backoff of up
to thirty seconds, from the offset it was told to read from next, so a reconnection neither repeats
nor skips a record. `--json` prints each record as the API's own object. What it does not do yet:
read from the node a retired workload's stream (question 6 of `14-streams.md`), or anything
already pruned from it. `korpis logs mc-survival --offloaded [--from N] [--limit N]` reads those
from the repository holding the workload's offloaded segments, through the control plane, which
records the read (§4 of `14-streams.md`); it prints offsets in no offloaded segment as missing, and
says where offloaded records end. It neither follows nor searches. `korpis stream mc-survival`
shows the stream as the control plane indexes it: how much its node keeps, how long its offloaded
segments are kept and the operator's floor beneath that (§8 of `17-security.md`), how much is held
only on the node, and which offsets are lost. `korpis stream share mc-survival --from 14:30
[--to 14:45] [--expires 24h] [--uses 3]` makes a share link to read what the console wrote between
the two times and nothing else (§8 of `14-streams.md`); a time is RFC 3339 or a time of day today
in the local zone, and `--to` is now when absent. It prints the link once, and the command its
recipient reads it with.

**`console` as built** (Phase 1): `korpis console mc-survival [--scrollback N] [--read-only]
[--relay URL]` opens two sessions at a relay: one reading the stream from its last hundred records
(or `N`) and following it, and, where the token may type, one writing input (§6 of
`14-streams.md`). Reading and writing are separate actions, so a caller who may only watch is
attached read-only and told so. On a terminal the command puts it in raw mode, so every key
reaches the workload as typed, sends the terminal's size and each change of it, and detaches on
`Ctrl-]`; output is written as the workload wrote it, line ends included. With input from a pipe
it sends what the pipe holds, closes its input, shows two more seconds of output, and ends, so
`echo save-all | korpis console mc-survival` is a script's way to type a command. Both sessions
renew their tokens on the session, and a lost one is opened again, the reader from the offset it
had reached.

`version` names the exact source revision each binary was built from and where its source is
published, for the client and for the control plane it is talking to. That is how an unmodified
distribution meets the AGPL's obligation for conveyed copies without anyone doing anything (§2 of
`19-governance.md`).

`files sync` transfers through a relay with a short-lived scoped token rather than through the
control plane, for the same reason web upload does (§8 of `11-surface-web.md`): a 40 GB world
should not traverse the control plane, and the control plane should not be a data path. Where the
node advertises a direct endpoint the CLI uses it, because the CLI pins the Korpis CA; the relay
path always works (§3.1 of `02-architecture.md`).

`event subscribe --kind workload.` keeps a position in the event log for you (§5 of
`15-observability.md`); `event follow` prints what it is delivered as it comes, and with `--ack`
records each line it printed as processed, so the next `follow` resumes after the last one, and
a line it did not get to print is delivered again. `event list` and `event delete` complete the
set.

`exec` is not universally available and does not pretend to be. §4.3 of `04-runtimes.md` forbids
the driver interface from assuming a shared filesystem or a POSIX process model, so `exec` exists
where the driver declares the capability and returns `UNSATISFIABLE`, naming the reason, where it
does not. The alternative (a command that silently means different things on different tiers) is
worse.

---

## 9. Open questions

1. **Multi-context.** `kubeconfig` is widely disliked and universally copied. A single context with
   explicit `--server` flags may be enough given that most operators run one control plane, but a
   consultant with ten clients is a real user. → here
2. **Config file format and location.** Follows from the above. → here
3. **CLI plugins.** `korpis-foo` on `PATH` becoming `korpis foo` is cheap and gives extensions a
   CLI surface for free, but it also creates a path-hijacking vector worth thinking through. →
   `16-extensions.md`
4. **Templating in declared files. Resolved here: none.** Every declarative tool grows one. Helm,
   Kustomize, jsonnet, and every one is regretted. Recipes already provide typed, schema-validated
   parameterization (§5 of `09-recipes.md`), so the honest answer may be that the CLI needs none,
   and the files are plain data. It was routed to `09-recipes.md`, which never carried it (finding
   S18). Declared files are plain data, validated against the schema. A team that wants to generate
   them uses whatever generator it already trusts and commits the output, which keeps the reviewed
   artifact identical to the applied one. A template language inside the files would be a second
   configuration language with its own evaluation rules, reviewed by nobody.
5. **Plan signing. Resolved here: the approval is the binding.** A Plan approved in a pull request
   and applied by a pipeline would ideally carry a signature binding the approval to the exact
   plan. That is an approval-workflow feature and belongs with governance rather than being
   invented here. It was routed to `19-governance.md`, which never carried it (finding S18). A Plan
   is an immutable object stored by the control plane under its identifier, and approving it is
   `plan.approve` recorded as an effect naming the approver's grant (§3.6 of `08-identity.md`). A
   pipeline applying `--plan pl_3c81` applies that object and nothing else, and a plan that changed
   would have a different identifier. The pull request links the plan identifier; the reviewer
   approves in Korpis, not by a comment Korpis cannot see. No separate signature adds anything the
   effect log does not already hold.
