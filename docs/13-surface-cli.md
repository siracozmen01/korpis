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
new workload; `pids` among the resources; `path`, `mode`, and `existing` on a volume; `protocol`
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
                                                 (pass --delete-volumes to remove)

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

### What a pipeline does when it arrives at drift

> Finding 18 of `23-walkthroughs.md`.

"Reconciled or adopted" describes a person at a terminal. §7 calls CI the good case for a scoped
token, and a CI runner can do neither: adopting means editing the repository, which a pipeline
should not do to itself, and reconciling means discarding the overnight fix, which is the thing
`advisory` exists to prevent.

Three behaviours are defensible and picking none means implementations pick differently and
operators find out by being surprised. So:

**An `advisory` set with drift fails the apply and applies nothing.** The failure names each
drifted object and the `Effect` that caused the drift, with its author and timestamp.
`--accept-drift` proceeds while leaving the drifted objects untouched, for the pipeline whose
operator has decided the disagreement is known and tolerated.

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
or from a file named with `--token-file`, the control plane from `KORPIS_SERVER` or `--server`,
and its certificate authority from `KORPIS_CA` or `--ca`, a PEM file trusted alone for it, so no
public authority can vouch for a Korpis control plane. Where the token comes from is the
operator's decision and the operator's storage; the CLI writes nothing down.

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

Shell completion resolves against what the caller can actually read, for the same reason Discord's
autocomplete does (§8 of `12-surface-discord.md`): existence follows read authority, so completion
cannot become a name-disclosure oracle.

---

## 8. The interactive commands

```
korpis console mc-survival           attach to the durable stream; scrollback, seek, replay
korpis logs mc-survival --since 1h   read without attaching
korpis exec mc-survival -- /bin/sh   a session, subject to the runtime's isolation tier
korpis files sync ./world w:mc-survival/world   delta transfer, through a relay
korpis forward mc-survival:25565     local port through the overlay, no public exposure
korpis watch -f servers/             stream plans and effects as they occur
korpis version                       client and server versions, and the source revision of each
```

`version` names the exact source revision each binary was built from and where its source is
published, for the client and for the control plane it is talking to. That is how an unmodified
distribution meets the AGPL's obligation for conveyed copies without anyone doing anything (§2 of
`19-governance.md`).

`files sync` transfers through a relay with a short-lived scoped token rather than through the
control plane, for the same reason web upload does (§8 of `11-surface-web.md`): a 40 GB world
should not traverse the control plane, and the control plane should not be a data path. Where the
node advertises a direct endpoint the CLI uses it, because the CLI pins the Korpis CA; the relay
path always works (§3.1 of `02-architecture.md`).

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
