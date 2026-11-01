# Recipes

Korpis's package format: what a recipe is, how it is distributed and trusted, how a declaration is
completed from it, and how it installs. Configuration fields and templating are on [recipe
configuration](recipe-config.md). It is called a recipe and nothing else; the name of Pterodactyl's
extension framework is never used for it, since the two would be confused in one market.

## Contents

- [What is wrong with the egg](#what-is-wrong-with-the-egg)
- [The artifact](#the-artifact)
- [Structure](#structure)
- [Completing a declaration](#completing-a-declaration)
- [The install language](#the-install-language)
- [The install worker](#the-install-worker)
- [Fetching](#fetching)
- [When an install runs](#when-an-install-runs)
- [Step providers](#step-providers)
- [Trust](#trust)
- [Locking and rollout](#locking-and-rollout)
- [Importing eggs](#importing-eggs)

## What is wrong with the egg

An egg is a JSON document carrying a bash install script that runs in a container with network
access and downloads whatever it needs at install time.

| Property | Egg | Consequence |
|---|---|---|
| reproducible | no | the same egg gives different bytes on different days |
| signed | no | an imported community egg is unauthenticated code |
| survives upstream | no | the URL dies, the egg dies |
| install mechanism | arbitrary bash | GHSA-p744-4q6p-hvc2 (CVE-2023-32080, critical): escape to the host from the installation container |
| templating scope | ambient | GHSA-pfvc-3p5h-x7h6 (critical): templating could render the daemon token and registry credentials into a tenant-readable file |
| configuration | untyped environment variables | no validation, generated forms or per-field permissions |

Importing a community egg is, in practice, running a bash script with privileges near root. Two of
Wings' published advisories exist only because arbitrary install code and ambient templating exist;
removing them removes those classes, not those bugs.

## The artifact

**A recipe is an OCI artifact**, pushed to any OCI registry: content addressing, immutability,
mirroring, signing policies, private registries and every registry tool already exist, so Korpis
operates no store, takes no cut and curates nothing. **The digest is the identity**: `name:version`
is a mutable pointer resolved at declaration, and the intent records the digest, so two workloads
from one digest run the same bytes.

The artifact is an OCI image manifest in the form the image specification's artifact guidelines give
(v1.1): `artifactType` `application/vnd.korpis.recipe.v1`, the empty config descriptor, and the
layers: exactly one document (`application/vnd.korpis.recipe.document.v1+yaml`, at most 1 MiB) and
each file the recipe refers to, named by `org.opencontainers.image.title`, its relative path with no
`..`. Resolving reads the manifest, the document and the message catalogues (at most 200, 256 KiB
each) and nothing else, checking the manifest's digest, each layer read against its descriptor, the
artifact type, the single document, and that every path the document names is in the manifest; a
recipe failing any does not load. Other files are read by whatever installs it, against the
manifest's digests. An extension's artifact has the same layout under types of its own, and type and
document `kind` must agree, since a registry is free to serve either and the kind decides whether
the thing is declared or installed.

This is the shape the specification recommends, so `oras`, registry garbage collection and referrers
(where signatures attach) treat a recipe as a first-class artifact; `scripts/e2e-phase0.sh`
publishes with an unmodified `oras push`. A second format is a new media type, never a
reinterpretation, so a control plane that does not know one refuses it by type. A registry refusing
unknown artifact or layer types cannot hold a recipe, and there is no fallback layout, which would
be a second thing to verify. The one implementation is the `korpis-recipe` crate, shared by the
control plane and the CLI.

**Thin and fat.** A thin recipe references external artifacts by URL and hash and dies if upstream
dies; a fat one carries them as layers and is self-contained. Both are supported, and a node's
content store keeps what a thin recipe fetched, which an operator can push to their own registry, so
an operator running a fat mirror is immune to an upstream URL disappearing, the failure that quietly
kills eggs.

## Structure

```yaml
apiVersion: korpis.dev/v1
kind: Recipe
metadata: {name: paper, version: "1.21.4", description: ..., locale: en, license: GPL-3.0}
runtime:
  targets:                                  # per tier and architecture
    - {tier: container, arch: [amd64, arm64], image: docker.io/library/eclipse-temurin@sha256:...}
    - {tier: process, arch: [amd64], rootfs: layers/rootfs.tar.zst}
  entry: {command: [...], working_dir: /data}
preset:                                     # the shape
  lifecycle: persistent
  console: interactive
  endpoints:
    - {name: game,  protocol: tcp, port: 25565, exposure_default: stable}
    - {name: query, protocol: udp, port: 25565}
    - {name: rcon,  protocol: tcp, port: 25575, exposure_default: overlay_only}
  volumes: [{name: data, path: /data, size_default: 10Gi}]
  resources: {memory: {min: 1Gi, recommended: 4Gi}, cpu: {min: 500m, recommended: 2000m}}
config: {fields: [...]}                     # recipe-config.md
install: {steps: [...]}                     # a closed verb list, no code
templates: [{from: templates/server.properties, path: /data/server.properties,
             format: properties, vars: [motd, max_players, difficulty]}]
managed_files: [{name: limits.properties, from: managed/limits.properties, format: properties,
                 vars: [max_view]}]
probes:
  readiness: {kind: query, protocol: korpis/minecraft, endpoint: query, timeout: 5s}
logs:
  events: [{pattern: 'Done \(.*\)! For help', emit: ready}]
lifecycle_hooks:
  pre_stop: {kind: console_write, data: "stop\n", wait_for_exit: 120s}
disruption:                                 # this software's layer of classification
  stop: {disruptive: true, reason: "disconnects players"}
  reconfigure: {disruptive_fields: [max_players, difficulty]}
```

A query protocol names the [health](health.md) provider answering it, `<publisher>/<protocol>`,
pinned at declaration as a step kind is; a probe naming one no installation answers refuses the
declaration.

**`pre_stop` exists because `SIGTERM` may not flush a world.** Writing `stop` to the console and
waiting is the difference between a clean shutdown and a corrupted save, declared by the recipe
rather than special-cased in core. The signed assignment carries the recipe document, and every stop
of a running object, whatever needed it, first writes the hook's data to the console as
`system:reconciler` (recorded in the stream and journaled as `node.pre_stop`), gives the process
`wait_for_exit` to exit by itself, and only then signals it, killing it after a grace of thirty
seconds. A workload whose console takes no input cannot be asked, which the effect records as
failed, and is signalled at once. A plan's stop steps carry `wait_for_exit` plus the grace as
`stops_within_seconds`.

**`disruption` is the recipe's layer** of [classification](plans.md#where-disruptive-comes-from): it
may only make a step more disruptive than its driver says, and fields in `disruptive_fields` make a
configuration change touching them disruptive. A recipe declaring nothing inherits the driver's.

## Completing a declaration

The control plane completes a declaration against its recipe, resolved to its digest, at
declaration, and the intent stores the completed body:

| Part | When the declaration leaves it out |
|---|---|
| `lifecycle`, `console` | the preset's; a preset naming no console asks for `logs` |
| `runtime.tier` | the first target's; a declared tier must have a target |
| `runtime.driver` | the Linux driver for the tier: `oci`, `native`, `firecracker`, `qemu` |
| `resources` | the preset's recommendation, reserved, when neither reservation nor limit is declared |
| `volumes`, `endpoints` | every preset entry; a declared one of the same name replaces what it states and inherits the rest |
| `health` | each probe not declared is the recipe's |

Whatever is declared must reach the preset's minimum, refused otherwise rather than placed to crash;
an endpoint with no exposure is `stable`. Completion is at declaration and on the server because an
intent is immutable and must mean the same forever, so it cannot depend on reading its recipe again,
and because one place performing it gives every client the same body, which lets `korpis plan` show
what will run. Entries merge by name because a recipe's probes name its endpoints, and adjusting one
endpoint should not mean restating the others. A declared list cannot remove a preset entry: an
unwanted endpoint is declared with exposure `none`, and a preset volume cannot be dropped, since the
software expects it. A workload for a non-Linux host must name its driver.

## The install language

**There is no step that runs arbitrary code**: no `run`, `exec`, `shell` or `script`.

| Verb | Arguments | Re-running it |
|---|---|---|
| `fetch` | `url`, `sha256` or `sha512`, `to` | no-op if the digest is held and at the target; else fetched or copied again |
| `copy` | `from` (a file of the recipe), `to` | overwrites the target, never merges |
| `extract` | `from`, `to`, `strip`, `include` | replaces the target tree, never merges |
| `mkdir` | `path`, `mode` | no-op if present with the mode; corrects the mode otherwise |
| `write` | `path`, `content`, `mode` | overwrites with the literal content |
| `template` | `from`, `to`, `format`, `vars` | renders from the source every time |
| `chmod`, `chown` | `path`, `mode`; `path`, `uid`, `gid` | sets the declared value |
| `symlink` | `target`, `link` | replaces a link; refuses to replace anything else |
| `delete` | `path` | no-op if absent |
| `<publisher>/<step>` | `to`, and the provider's own | re-resolves, and overwrites the targets as `copy` does |

**A `fetch` without a hash is a parse error**; the recipe does not load. `chown` takes ids as the
workload sees them, 0 being its own root and nothing above 65,535, so no value can name a host
identity, the mapping being applied at mount.

**Every verb states its own idempotence**, because an install is one plan step and several verbs,
and resuming at the plan step would refetch forty gigabytes over a directory deliberately kept,
which is how `extract` and `template` produce a workload that starts and is quietly wrong. The
closed verb list makes this cheap. Progress is recorded per install, so a resume continues at the
verb that failed.

**As built.** Each path is resolved beneath its volume by `openat2` with `RESOLVE_BENEATH`, no magic
links and no mount crossing, so a planted link makes a verb fail, never write elsewhere. A file is
written beside its target, synced and renamed over it, mode `0644` for content and templates.
`extract` recognises tar (bare, gzip, zstd, xz) and zip by their first bytes, refuses an absolute or
`..` entry, a device or FIFO, a link out of what it unpacks and a hard link to anything not unpacked
with it, unpacking nothing then; it unpacks into a fresh tree beside its target and swaps them,
never taking a volume's root as target, since swapping that would delete everything the workload
wrote. Unpacked files belong to the workload's root whatever the archive says. `chmod` refuses a
link; `chown` changes a link's own owner. `symlink` points within its own volume. `delete` never
deletes a volume's root. A recipe cannot unpack straight into a volume's root.

**Everything an install writes lands in a volume.** Every path a verb or template writes must be in
one of the workload's volumes, and a declaration whose volumes leave one outside is refused, naming
the step: a file written anywhere else would vanish with the instance, and a recipe relying on it
would install a workload that starts once. Nothing a recipe names lies at or beneath `/.korpis`,
where the node puts what it gives the worker to read. An upgrade keeping a file the tenant edited
has to say so by not writing it.

## The install worker

Every verb touching a volume runs in the workload's own confined worker, `korpis-install-job`,
started by the helper as a backup job is, as two processes. The first is root only while it builds
the second's world: it joins the job's cgroup and makes a PID namespace whose first process is the
worker, so the worker can name, signal or trace nothing outside it and dies with it. The worker
drops its supplementary groups and makes its own user, mount, network and IPC namespaces; the first
maps the worker's 0 to 65,535 onto the workload's own id range, checked against the helper's ledger,
mounts each volume as an idmapped copy, and builds a read-only tmpfs root holding only those volumes
at the workload's paths and the node's content read-only at `/.korpis/content`, with nothing of the
host, not even `/proc`. The worker becomes its user namespace's root, which is the workload's root
and no one else on the host, keeps four capabilities over the workload's files (setting owners and
modes), applies Landlock to its volumes, its output and the content, takes no new privileges and has
no network. Progress is written after every action and the result at the end, in a directory of its
own, so a lost worker is resumed by a new job at the action it did not finish, and what confined it
is in the result as the kernel reported.

The workload's own ids rather than root with few capabilities, because the worker reads what a
recipe supplies, archives above all, and nothing that accepts tenant input holds privilege; root
with four capabilities in the host's user namespace would be privilege, sharing an identity with
every root process on the node. It needs idmapped mounts, so on a storage class without them an
install fails and says why. It applies no seccomp filter, so a compromised decoder reaches the
kernel's whole system call surface, as the workload does; and the first process, root for moments,
decodes the spec, acting on none of it.

For a block device, the driver runs the same worker in a service guest from a content drive (see
[guests](guests.md#service-guests)); its zstd decoder (`ruzstd` 0.9.1, the first release bounding
what one block decodes to, RUSTSEC-2026-0344) refuses a frame asking a window over 128 MiB.

## Fetching

**A `fetch` is performed by the node's content store, not the worker.** The store downloads the URL
once per node, verifies the declared hash, keeps it by digest apart from the rest of the store, and
the worker copies it in, so a second install of the same file fetches nothing and the worker
handling tenant paths never has a network.

**A `fetch` reaches the public internet and the networks the operator names, nothing else.** The URL
is the recipe's, and anyone may write a recipe, so the request is one its author chose: the node's
loopback, the cloud metadata service, a service on the operator's network. The hash protects what is
kept, not the request, and how a fetch fails tells its author what answered. That is Pterodactyl's
SSRF class, one advisory in exactly this place (CVE-2024-34068). So the node connects only to public
addresses unless the operator allows a network (`--fetch-allow`): every address a name resolves to
is checked by the resolver the connection then uses, so a name cannot pass the check and connect
elsewhere; a literal address is checked before anything is sent; every redirect likewise. Loopback,
private, link-local, CGNAT, multicast, documentation and reserved ranges are refused, as is an IPv6
address carrying one of those. A mirror on a private network has to be named by the operator.

## When an install runs

An install is one plan step, `install`, in three cases: when a workload is created, after its
volumes exist and before its instance, every verb then every template; when its recipe changes
digest, the instance stopped, the new recipe's verbs and templates run over what the volumes hold,
and started again; and when values its templates use change, only those templates rendered again
before reconfiguring. A workload created from a backup is not installed, its volumes holding the
installed tree. The step's detail carries why it runs and every action in order, each verb with its
arguments and each template with its context, so the plan shows exactly what will be written and the
node needs nothing else of the recipe. An install writing only into volumes its own plan creates
writes over nothing and is reversible with them; every other install writes over what is there and
is irreversible, so its plan is confirmed.

The node first fetches what the actions read, each `fetch` by its hash and the recipe's own files
where a `copy`, `template` or `extract` reads them, then has the helper start the worker over the
volumes the workload mounts read-write. An install is named by a digest of the workload and its
actions, and its progress is kept per install: a failed worker's progress is kept and the step
fails, and a later plan carrying the same install starts at the action that failed; a lost worker is
started again by the same step, and three losses in a row fail it rather than looping.

## Step providers

Real installs sometimes need what a verb list cannot express, SteamCMD being what half this market's
games require. The answer is not scripts: an [extension](extensions.md) registers a provider for a
step kind, and the recipe uses it declaratively:

```yaml
install:
  steps:
    - valve/steam.app: {to: /data/server, appid: 730, branch: public, manifest: 8471...}
```

The provider runs in the extension's own sandbox under its own grant, not with the installer's
privilege, in the tenant's namespace or with core's authority. It only resolves the step into
content-addressed artifacts, which the node fetches and the workload's worker writes beneath `to`, a
path of the recipe's checked at declaration like every target. So no provider can write a volume,
and re-running a step can only re-resolve it and overwrite the same targets: the guarantee is the
interface's, holding for every provider rather than only honest ones. The step is named by publisher
and its provider pinned by digest in the lockfile. A step naming exact content, as `manifest` does,
is reproducible; one naming a channel gets whatever it holds today, and the workload is marked
unreproducible. "Two workloads from one digest run the same bytes" is true of reproducible recipes,
and the label says which those are.

## Trust

```
TrustPolicy               an organization's own, or its nearest ancestor's
  accepted_signers        cosign public keys (ECDSA P-256), each named; identity is the
                          SHA-256 fingerprint of its DER
  allow_unsigned          default false; every use logged
  allow_unverified        default false; imported eggs that kept arbitrary logic
```

- Recipes are signed with cosign, signatures in the registry in the form `cosign sign --key` writes:
  a manifest tagged `sha256-<hex>.sig` whose layers are simple-signing payloads naming the signed
  digest, counting only for that digest.
- A recipe is usable when an accepted signer signed its digest; a signature by a key the policy does
  not accept proves nothing to it.
- **The default policy runs nothing.** It accepts no signer and allows nothing unsigned, because
  Korpis ships no key it asks anyone to trust: an installation runs recipes once its operator has
  said whose.
- Verification happens before anything is fetched or unpacked.
- Registry credentials are organization-scoped secrets held by the control plane, never sent to a
  node, which receives short-lived digest-scoped pull tokens. A tenant may supply credentials for
  its own registry, which the operator can read, since the operator runs the control plane that uses
  them; a tenant needing otherwise gives a pull-only, revocable credential and revokes it when the
  relationship ends.
- What vouched for a recipe (an accepted signer by name, or nothing) is recorded in every intent
  declared from it.

**Which policy governs.** An organization's own, else its nearest ancestor's, else the default,
since the policy protects the organization running the recipe: a forged recipe endangers the
workload and its data, which are the organization's, while the host is protected by the isolation
tier. A reseller's customer who never thought about signing runs what the reseller runs.
`GetTrustPolicy` and `SetTrustPolicy` need `org.read` and `org.write`; a policy is changed by
compare-and-swap against the organization's own version, which keeps counting through a removal;
every change is an effect, `org.trust_policy.set`, naming each signer. A parent cannot impose its
policy as a ceiling: a child holding `org.write` over itself may accept what its parent does not.

**The control plane evaluates it at declaration, on the digest**, before anything of the recipe is
fetched, dry runs included. A recipe's signatures are kept in the store with it, so a rollback years
later is decided without the registry, which is asked again only when the kept signatures do not
satisfy the policy. The refusal is `recipe.untrusted` on `body.recipe`, which no grant changes. A
use under `allow_unsigned` is recorded in the intent and as an effect, `recipe.use_unsigned`. It is
decided once per intent: changing a policy touches no intent already declared, and withdrawing trust
from a running recipe is an action on its workloads.

## Locking and rollout

```yaml
# korpis.lock
recipes:
  - ref: korpis.io/minecraft/paper:1.21.4
    digest: sha256:9f2c...
    artifacts: [{url: "https://api.papermc.io/...", sha256: "a1b2...", vendored: true}]
    providers: [{step: valve/steam.app, digest: "sha256:5d0e..."}]
```

`name:version` resolves through the lockfile once, at declaration, and afterwards the intent refers
to a digest alone, so upstream retagging, deletion or compromise are irrelevant to a running
workload. Updating a recipe is an intent change with a plan; updating many workloads is a
`recipe_rollout` operation with `max_unavailable` and a window. Rollback is declaring the previous
digest.

## Importing eggs

The importer converts what is mechanically convertible (metadata, variables, the image, startup
command, configuration file mappings, stop signal) and analyses the install script:

| Result | Meaning |
|---|---|
| clean | the script reduced entirely to verbs: a normal recipe |
| hashable | fetches carried no hash; the importer fetches once and pins what it observed, saying so |
| unverified | arbitrary logic remains |

An `unverified` recipe is accepted only under `allow_unverified`, and its install runs in a microVM,
with no network but hosts the importer extracted and an operator confirmed, marked unreproducible
with every workload declared from it, and the operator's acknowledgement recorded as an effect. It
is deliberately worse to use than a real recipe and still safer than Pterodactyl, where the same
script runs in a container with unrestricted network and nobody's acknowledgement. The importer is
not built.
