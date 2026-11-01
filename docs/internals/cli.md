# CLI

`korpis`: one binary that calls only the published API, the reference client against which "every
client is a peer" is enforced mechanically. Every API operation has a command and no command has a
private endpoint. The daemons (`korpis-server`, `korpis-agent`) are separate binaries with their own
surface ([control plane](control-plane.md), [node agent](node-agent.md)).

## Contents

- [Coverage is checked](#coverage-is-checked)
- [Two ways to say the same thing](#two-ways-to-say-the-same-thing)
- [Output and exit codes](#output-and-exit-codes)
- [Names](#names)
- [Declared files are intent bodies](#declared-files-are-intent-bodies)
- [Plan is the real diff](#plan-is-the-real-diff)
- [Sets and pruning](#sets-and-pruning)
- [Drift](#drift)
- [Credentials](#credentials)
- [Inside a workload](#inside-a-workload)
- [Grants, templates and links](#grants-templates-and-links)
- [Extensions](#extensions)
- [Streams, consoles and files](#streams-consoles-and-files)
- [Completion](#completion)
- [The command families](#the-command-families)
- [What it does not do](#what-it-does-not-do)

## Coverage is checked

`korpis call WorkloadService/GetWorkload -d '{"id": "wl_..."}'` reaches every method by name, sends
the request as the API's JSON and prints the answer the same way, a server stream message by
message. Its table of methods is written by hand and a test compares it with the schema's list of
`korpis.v1` methods, so a method added without a command fails the build. The friendlier commands
are built over the same calls, never beside them. A mutating request without an idempotency key is
given a fresh one, so a retry the CLI makes is safe and two invocations are two requests
([api](api.md)).

## Two ways to say the same thing

```
korpis workload restart mc-survival        imperative: one object, now
korpis apply -f servers/                   declarative: a set of objects, converge
```

Both produce an intent, a [plan](plans.md) and the same audit trail. The imperative form is a
convenience over the declarative one, not a second path, which is why the two cannot drift apart the
way `kubectl edit` and a GitOps controller do. A power change is made against the power version the
CLI just read, so two operators stopping and starting the same workload at once see one `CONFLICT`
rather than both silently winning.

## Output and exit codes

Human output by default; `--json` emits the API's own object shape, so a script that outgrows the
CLI moves to the SDK unchanged. With `--json` a failure is written to standard error as the API's
error (code, reason, field, message).

| Exit | Meaning |
|---|---|
| 0 | done |
| 1 | refused by the CLI before anything was sent: a file, an argument, a value, an unresolved name |
| 2 | the command line does not parse |
| 3 | the control plane could not be reached, or answered without a structured error |
| 10 to 20 | `INVALID`, `UNAUTHENTICATED`, `DENIED`, `NOT_FOUND`, `CONFLICT`, `UNSATISFIABLE`, `QUOTA_EXCEEDED`, `OVERFLOW`, `UNAVAILABLE`, `INTERNAL`, `RATE_LIMITED`, in that order |

## Names

An object is named by identifier, by name, or for a workload as `project/name`. A name is resolved
by listing what the caller may read, never by a lookup the API does not offer, so a name the caller
cannot read is the same local refusal as one that does not exist and resolving learns nothing the
API would not say. Listing starts from the organizations the caller may read and the objects its own
grants are anchored at, so a token scoped to one project names that project. An organization's name
is found by walking down the tree from there, every list read to its last page, so a reseller names
its customers' customers as a host names its own. A name matching more than one object is refused
with advice to use the identifier. The cost is calls: a bare workload name lists every readable
project, and a caller who can read thousands of organizations pays a call for each; an identifier
avoids both.

## Declared files are intent bodies

A declared file is the intent body, with no `apiVersion`, `kind` or metadata wrapper:

```yaml
workload: mc-survival
project: community-servers
recipe:  ghcr.io/korpis/recipes/minecraft-paper@sha256:9d2f...
lifecycle: persistent
console: interactive
resources:
  memory: { reservation: 4Gi, limit: 6Gi }
  cpu:    { reservation: 2 }
volumes:
  - { name: world, size: 40Gi, class: local, replicas: 1, replication_interval: 5m }
endpoints:
  - { name: game, port: 25565, exposure: stable }
continuity:
  replica_failover: automatic
config:
  motd: "Survival, season 4"
  difficulty: hard
```

Other keys: `power` and `labels` for a new workload; `pids` among resources; `path`, `mode`,
`existing`, `from_backup`, `replicas` and `replication_interval` on a volume; `continuity` with
`on_lease_expiry` and `replica_failover`; `protocol` and `domains` on an endpoint; and `placement`
(`node_selector`, `tolerations`, `affinity`, `preferred_region`, see [placement](placement.md)).
Quantities are `4Gi` or `500m`; enum values are the API's, lower case without prefix. The project is
a name unique among those the caller reads, or a `prj_` identifier. `-f` takes a file or a
directory, whose `.yaml` and `.yml` files are read in name order. An unknown key is refused, never
ignored.

What the file leaves out (runtime, probes, mount paths) the recipe's preset supplies on the control
plane ([recipe config](recipe-config.md)), and the plan shows the completed body. The check before
sending is the control plane's own code run locally: the recipe is fetched by digest, the
declaration completed and the configuration resolved against its schema, so a typo is
`servers/mc.yaml:5: config.greting: not declared by the recipe`. What only the control plane can
decide (authority, quota, placement) it decides on arrival.

Declared files are plain data with no templating. Recipes already give typed, schema-validated
parameterization; a team that wants to generate files uses a generator it trusts and commits the
output, so the reviewed artifact is the applied one. A template language in the files would be a
second configuration language reviewed by nobody.

## Plan is the real diff

`korpis plan -f servers/` sends the declaration with `propose` and receives the real, persisted
plan, computed by the code that will run it, with placement, explanations, quota effects and
estimated unavailability, not a text diff computed on the client:

```
Plan pl_3c81 · 3 changes
  ~ mc-survival   memory 4Gi -> 6Gi       restart required, ~40s, 14 players connected
  + mc-creative   create on node fra-3    fra-3: most free memory after packing
  - mc-testing    delete                  volume "world" 12Gi will be RETAINED
quota  community-servers   memory 18Gi -> 24Gi of 32Gi
apply with:  korpis apply -f servers/ --plan pl_3c81
```

A proposed plan waits for confirmation even where default policy would approve it at once. Plans are
per workload, so a directory gives one plan each and `--plan` is given once per workload; planning
again supersedes the plan before it. `apply --plan` declares the same files as a dry run, compares
the completed body with the one the plan was computed for, and confirms that plan; a difference is
`CONFLICT` with the new plan shown, so a plan reviewed in a pull request is the plan that runs.
Without `--plan`, `apply` declares and default policy approves what it may.

No plan signature exists, because the approval is the binding: a plan is immutable under its
identifier, approving it is `plan.approve` recorded as an effect naming the approver's grant, and a
changed plan has another identifier. The pull request links the plan; the reviewer approves in
Korpis.

## Sets and pruning

A declared set has an owner, and removal from the set is a proposal, never an act
([state](state.md)). `korpis apply -f servers/ --set community-servers` marks what it creates with
the set, named within each project the files declare into and created by the first apply. A member
no longer in the files appears in the plan as a deletion, enumerated by name:

- Deleting a workload is a retire plan, which always waits for a person: `plan --set` proposes it,
  `apply --set` proposes and confirms it, and `apply --plan` deletes exactly the members whose
  retire plans it names.
- Nothing holding data is deleted by an apply. Volumes, snapshots and backups are kept unless
  `--delete-volumes` names each as `member/volume`; a name for a member the apply does not delete is
  refused, because deleting data is never inferred from a matching name. Each named volume becomes
  its own irreversible step under `volume.delete`, marked `IRREVERSIBLE`, and a backup's retention
  floor cannot be overridden ([backup](backup.md)).
- An object that never belonged to the set is never touched by it, so two sets cannot fight and a
  pipeline cannot prune a hand-made workload. `--adopt` takes in a workload that belongs to no set;
  without it such a workload is refused, not silently claimed.

Every file is declared as a dry run through the set first, so drift fails the apply before the first
change rather than after half of them.

## Drift

Files declare intent, and so do the panel and chat; a controller silently reverting a 03:00 outage
fix four minutes later is the GitOps failure this avoids. Every intent carries `managed_by`, drift
is derived from that pointer rather than stored, and each set has a mode:

| Mode | Behaviour |
|---|---|
| `advisory` (default) | the out-of-band change stands; the set is drifted, shown in the panel and `korpis status`, and the next plan shows it |
| `strict` | the next apply declares over it, and the discarded intent is recorded as an effect with its author |

The control plane decides, because any client can declare through a set: a body equal to the drifted
head is the files catching up and is accepted in either mode; anything else is declared over by a
`strict` set and refused `CONFLICT` reason `set.drifted` by an `advisory` one unless it says
`reconcile`. An `advisory` set with drift fails the whole apply and applies nothing, naming each
drifted object and the effect that caused it with its author and time. `--accept-drift` proceeds
leaving drifted objects untouched; `--reconcile` declares over them, recording each discarded
change. Failing the whole apply is deliberate: a partial apply against a set someone is editing by
hand is how two sources of truth become three, and a pipeline going red at 09:00 because a person
fixed something at 03:00 is the correct outcome. `korpis status` shows a drifted member's author and
effect and every quota-bearing figure on which drift and files disagree. `set list` and `set mode`
read and change a project's sets.

## Credentials

No long-lived admin key in a dotfile. `korpis login` runs the device-code flow
([authentication](authentication.md)): it prints the panel's device page and a code on standard
error and polls until the code is approved there. A person who signs in through an identity provider
does the same, approving on the panel after signing in there, so no redirect address is registered
for the CLI. The session is kept in the system keyring (Secret Service on Linux, the login keychain
on macOS) under `korpis` and the control plane's URL, or, where none is reachable or
`KORPIS_KEYRING=off`, in a file under `$KORPIS_CONFIG_DIR` only its user can read, which `login`
says it did and why.

Every command exchanges the kept credential for an access token of 120 seconds while holding a lock
beside it, so concurrent commands take turns instead of presenting one credential twice (which ends
the session), and writes back the replacement; the access token is kept until thirty seconds before
expiry, and a long command renews its own. `korpis login --no-store -- <command>` signs in, runs one
command with the session in memory, and signs out; `logout` ends the session and forgets it. A
credential copied from the file and used elsewhere ends the session at the first double exchange,
which is how a copy is detected.

For automation, `KORPIS_TOKEN` or `--token-file` (`KORPIS_TOKEN_FILE`) takes precedence and the CLI
writes nothing down. `KORPIS_SERVER` or `--server` names the control plane, and `KORPIS_CA` or
`--ca` a PEM file trusted alone for it, so no public authority can vouch for a Korpis control plane.

## Inside a workload

With no server named and `/run/korpis/identity.sock` present, or with `--identity`
(`KORPIS_IDENTITY`), the command asks the socket for the workload's credential, asking again for up
to a minute while the socket says to (until the node holds the workload's lease). It trusts the CA
the socket names, reaches the control plane at the address it names, through the relay the node
serves beside the socket where there is one, and renews the credential five minutes before expiry.
So a nightly backup's run holds exactly the grants issued to the workload, and nothing is written
into the workload's configuration. Through the relay it reaches the control plane alone; a console
or file relay elsewhere is refused with why. `korpis heartbeat [--every 30s]` beats on the socket
for a heartbeat probe ([health](health.md)) and needs no credential.

## Grants, templates and links

```
korpis grant issue --actions workload.read,workload.write --scope project:community-servers \
  --expires 90d --label "github actions, korpis-community/servers"
```

The grant goes to a token made to hold it alone, attenuated from the one grant the issuer holds that
it is valid from, and the secret is printed once. It deploys one project, expires on a date, appears
in every effect it authorizes, cannot mint a broader grant, and is checked online on every request,
so revoking it stops the pipeline's next call ([grants](grants.md)). `--to` issues to an existing
subject; `grant list`, `show` (with its chain) and `revoke` complete the set. `--template Moderator
--scope project:survival` applies a role: the template gives actions and delegation at its current
version unless `--template-version` names one, and `--expires`, `--source-cidr` and
`--requires-approval-by` replace its defaults. `template create|list|show|edit|retire|holders`
manage roles, the file holding what `template show --json` prints, edited against the version it was
read at.

`korpis grant share --actions workload.restart --scope workload:mc-survival` prints a share link
once as a URL with the link in its fragment, saying what it permits, on what, how many times, for
how long, and that using it needs the control plane ([capability tokens](capability-tokens.md)).
`--expires` defaults to `24h` and `--uses` to `3`, and neither can be removed. `KORPIS_TOKEN` takes
the link or the whole URL as pasted. A CI token is not a link, because every use of a link spends
one of a cap it cannot go without.

`identity bind|list|show|edit|retire|verify` manage [external identity](external-identity.md)
bindings, printing interval and grace with their sum, the longest an unreachable provider extends
one. `organization ceiling` reads or sets the delegation ceiling and `organization placement` the
placement policy, each showing what is in force and the organization's own.

## Extensions

Installing an extension is issuing grants, so it is two steps like `plan` and `apply`. `korpis
extension install ARTIFACT --version 1.0.0` changes nothing: it prints what the extension needs,
over what and why, what it reaches, provides and subscribes to, and the command approving exactly
that. `--approve` is refused unless `--version` is a digest, since a tag may move between reading
the needs and approving them. `upgrade` is two steps the same way, showing what would be kept,
issued and revoked. `extension providers` lists provider streams, and `extension ping dns
cloudflare` calls one's liveness check and prints how it ended ([extensions](extensions.md)).

## Streams, consoles and files

```
korpis console mc-survival           attach: scrollback, live output, input where allowed
korpis logs mc-survival --since 1h   read without attaching
korpis exec mc-survival -- /bin/sh   where the driver declares exec; UNSATISFIABLE otherwise
korpis files sync ./world w:mc-survival/world
korpis sftp mc-survival              where an SFTP client connects, and the host key to expect
korpis event follow sub_... --ack    a subscription's events, acknowledged as printed
korpis version                       client and server versions and source revisions
```

Each mints a scoped token and opens a session at a [relay](relays.md), never through the control
plane, renewing its token on the session thirty seconds before expiry; a lost session reopens with a
backoff of up to thirty seconds from the offset it had reached, so nothing repeats or is skipped.

- **`logs`** reads in offset order from the node's files, searched there with `--grep`; gaps print
  as gaps and offsets the node no longer holds say so. `--offloaded` reads offloaded segments
  through the control plane, which records the read. `stream` shows the stream as indexed, and
  `stream share --from 14:30 --to 14:45` makes a link to read that window alone
  ([streams](streams.md)).
- **`console`** opens a reading session and, where the token may type, a writing one; a watcher is
  attached read-only and told so. On a terminal it is raw mode with the terminal's size sent and
  `Ctrl-]` to detach; from a pipe it sends the input, shows two more seconds and ends, so
  `echo save-all | korpis console mc-survival` is how a script types.
- **`files ls|get|put|sync|mkdir|mv|rm`** run over one file session ([files](files.md)). A remote
  path is `w:WORKLOAD/PATH`, or `w:WORKLOAD:PATH` when the workload is `project/name`. A mebibyte
  per request, four ahead; `--resume` continues from what is there, each chunk landing at the file's
  end. A download is named after the path asked for, never a name the node answered. `sync` makes
  directories and sends files the workload lacks at the same size and at least as new, follows no
  symbolic link at any depth, reads a file only if it is still the one it looked at, and removes
  nothing; it misses a change keeping size and time and sends changed files whole, because a file
  session carries no digest. Names, refusals and end reasons are printed with control characters
  escaped.
- **`sftp`** prints, per relay serving SFTP, the client line, the host key's SHA-256 fingerprint and
  a `known_hosts` line, so the first connection is checked. `ssh-key add` registers a key with its
  signature over the account (made with `ssh-keygen -Y sign`, or `--signature FILE`) and needs a
  recent sign-in, obtained on the panel's device page when the terminal's is older.
- **`version`** names the source revision and publication of the client and of the control plane it
  talks to, which is how an unmodified distribution meets the AGPL for conveyed copies.

## Completion

`korpis completion bash|zsh|fish|elvish|powershell` prints a script registering the binary as its
own completer, meant to be sourced at shell start, since the protocol between them is the binary's.
Each completion lists through the API exactly as name resolution does, so completion is no
name-disclosure oracle, and nothing is cached, so a revoked grant completes nothing on the next key.
Each completion is a process and a few calls, a pause on a slow link, and sees the environment, not
flags typed earlier on the line. A failure completes nothing and prints nothing.

## The command families

Each is one API call once its names are resolved, and its page is where the behaviour lives:

| Commands | Page |
|---|---|
| `get organizations\|projects\|workloads\|nodes\|plans\|effects`, `status` | [api](api.md), [observability](observability.md) |
| `organization create\|show\|suspend\|resume`, `project create` | [tenancy](tenancy.md) |
| `quota show\|set` | [quota](quota.md) |
| `plan approve\|reject\|cancel`, `workload start\|stop\|restart\|place\|classify\|delete\|recipe` | [plans](plans.md), [workloads](workloads.md) |
| `workload move`, `migration get\|abandon\|rollback\|release` | [migration](migration.md) |
| `run list\|get\|cancel\|logs` | [runs](runs.md) |
| `node token create\|cordon\|uncordon\|remove` | [node agent](node-agent.md) |
| `volume delete\|snapshot\|snapshots\|rollback\|delete-snapshot` | [storage](storage.md) |
| `repository`, `backup create\|list\|get\|restore\|ls\|cat\|clone\|prune` | [backup](backup.md) |
| `domain`, `certificate`, `portal`, `address-pool`, `edge-group` | [endpoints](endpoints.md), [portals](portals.md), [addresses](addresses.md), [edges](edges.md) |
| `get usage\|periods` | [metering](metering.md) |
| `event subscribe\|follow\|list\|delete` | [observability](observability.md) |
| `theme`, `layout` | [branding](branding.md) |

## What it does not do

- One context only: `--server` and the environment. Several contexts for a consultant with ten
  control planes, and a configuration file to hold them, are not designed.
- No CLI plugins: `korpis-foo` on `PATH` becoming `korpis foo` is a path-hijacking vector not yet
  thought through.
- `logs` does not read a retired workload's stream from the node, nor anything pruned.
- `files sync` uses no direct node endpoint yet; it waits on nodes advertising one.
- `status` does not show a migration's phase yet.
