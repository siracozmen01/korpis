# Recipes: The Package Format

**Status:** design **Date:** 2026-08-07 **Depends on:** [`01-model.md`](./01-model.md),
[`04-runtimes.md`](./04-runtimes.md), [`06-storage.md`](./06-storage.md) **Resolves:** open
question 4 of `04-runtimes.md` **Implements:** Rules K-2, K-15, K-16, K-17

> **On the name.** "Blueprint" is taken: it is the name of Pterodactyl's de facto extension
> framework (blueprint.zip). Using it for a package format in the same market would be confusing
> and discourteous. `Recipe` is free here. Pterodactyl and Pelican use `Egg`, AMP uses templates,
> Helm uses charts, Nix uses derivations, Docker uses images.

---

## 1. What is wrong with the egg

An egg is a JSON document carrying a bash install script. That script runs in a separate container
with network access and downloads whatever it needs at install time.

| Property | Egg | Consequence |
|---|---|---|
| Reproducible | no | the same egg produces different bytes on different days |
| Versioned content | no | "latest" from an upstream URL, whatever that is today |
| Signed | no | an imported community egg is unauthenticated code |
| Survives upstream | no | the URL dies, the egg dies |
| Dependency resolution | none | none |
| Install mechanism | arbitrary bash | GHSA-p744-4q6p-hvc2 (CVE-2023-32080, critical): escape to host from the installation container, reachable by anyone who can modify an install script or influence the user data one executes |
| Templating scope | ambient | GHSA-pfvc-3p5h-x7h6 (Critical): egg configuration-file templating could render the daemon token and registry credentials into a tenant-readable file |
| Config typing | untyped environment variables | no validation, no generated forms, no per-field permissions |

Importing a community egg is, in practice, "run this bash script with privileges near root." Very
few people treat it that way.

Two of the published Wings advisories (§1.1 of `research/evidence.md`) exist *only* because
arbitrary install code and ambient templating exist. Removing them removes those classes, not those
bugs. An earlier version of this sentence said "two of the ten" and listed the first as two
separate issues; it is one advisory with a GHSA and a CVE identifier, and the published total is
higher than ten (finding S61).

---

## 2. The artifact

**A recipe is an OCI artifact.** Not a new distribution system, an OCI artifact pushed to any OCI
registry.

```
korpis.io/minecraft/paper:1.21.4  →  sha256:9f2c…
```

This is free infrastructure, and it is the reason `00-overview.md` §2 can say Korpis is not a
marketplace: content addressing, immutability, mirroring, mandatory-signing policies, CDN, private
registries, authentication, retention, and every existing registry tool already exist. Korpis
operates no store, takes no cut, and curates nothing.

**The digest is the identity.** `name:version` is a mutable pointer resolved at declaration time;
the `Intent` records the digest (§3.5 of `01-model.md`). Two workloads declared from the same
digest run the same bytes, permanently.

### The layout

The artifact is an OCI image manifest in the form the image specification's guidelines for
artifact usage give (image-spec v1.1): `artifactType` is `application/vnd.korpis.recipe.v1`, the
config is the empty descriptor (`application/vnd.oci.empty.v1+json`), and the layers are the
recipe's content.

| Layer | Media type | Named by |
|---|---|---|
| the document, exactly one | `application/vnd.korpis.recipe.document.v1+yaml` | |
| each file the recipe refers to | what the file is | `org.opencontainers.image.title` |

A file's title is its path inside the artifact, relative, with no `..` component; the document
names files by those paths (a template's `from`, an install step's `from`, a target's `rootfs` or
`disk_image`). The document is at most 1 MiB.

Resolving a recipe reads the manifest, the document, and the recipe's catalogues (§5.3), and
nothing else. The control plane checks the manifest's digest against the one it asked for, each
layer it reads against its descriptor, the artifact type, that there is exactly one document
layer, and that every path the document names is a file the manifest carries. A recipe that fails
any of these does not load. The other files are read by whatever installs the recipe, against the
digests the manifest gives.

> Corrected 2026-10-17, when the catalogues were built. This paragraph said resolving reads the
> manifest and the document and nothing else, which §5.3 contradicted: a catalogue checked "when
> it resolves the recipe" has to be read then. The catalogues are read at resolve, bounded (at most
> 200 of them, each at most 256 KiB), because a translation that breaks a message must keep the
> recipe from loading where someone can still fix it, not fail on a screen later; every other
> file is still left to the installer.

An extension's artifact has this layout under types of its own, and its document's `kind` is
`Extension` (§3.1 of `16-extensions.md`). The artifact type and the document must agree: a
manifest typed as a recipe whose document is an extension's, or the reverse, does not load,
because a registry is free to serve either and the kind decides whether the thing is declared or
installed.

Why this shape: it is the one the specification recommends, so `oras`, registry garbage
collection, and referrers (where signatures attach, §7) all treat a recipe as a first-class
artifact without knowing what Korpis is. This is checked rather than assumed:
`scripts/e2e-phase0.sh` publishes its recipe with an unmodified `oras push`, and the control plane
resolves and runs it. The `v1` in both media types is the format version: a
second format is a new media type, never a reinterpretation of the old one, so a control plane
that does not know a format refuses it by type instead of misreading it.

What it costs: a registry that refuses an unknown `artifactType` or unknown layer media types
cannot hold a recipe, and Korpis does not fall back to another layout for it, because two layouts
would be two things to verify. The one implementation of the layout is the `korpis-recipe` crate,
shared by the control plane and the CLI (§2 of `../ARCHITECTURE.md`).

### Thin and fat recipes

| | Contains | Size | Durability |
|---|---|---|---|
| **thin** | references to external artifacts by URL + hash | kilobytes | dies if upstream dies |
| **fat** | the artifacts themselves as OCI layers | large | self-contained forever |

Both are supported, and Korpis can **fatten** a thin recipe: on first successful install the
fetched artifacts are stored by hash in the node's content store and optionally pushed to the
operator's own registry, producing a fat recipe with an identical digest for its declared content.

This closes the failure mode that quietly kills eggs, an upstream URL disappearing and taking every
server that depended on it with it. A provider running a fat mirror is immune to the rest of the
internet.

---

## 3. Structure

```yaml
apiVersion: korpis.dev/v1
kind: Recipe

metadata:
  name: paper
  version: "1.21.4"
  description: PaperMC server
  maintainers: [...]
  license: GPL-3.0
  annotations: {...}

runtime:
  targets:                                  # per tier and architecture
    - tier: container
      arch: [amd64, arm64]
      image: docker.io/library/eclipse-temurin@sha256:…
    - tier: process                         # resolves open question 4 of 04-runtimes.md
      arch: [amd64]
      rootfs: layers/rootfs.tar.zst        # a file of the artifact (§2), unpacked
  entry:
    command: [...]
    working_dir: /data

preset:                                     # the "shape" (§2 of 01-model.md)
  lifecycle: persistent
  console: interactive
  endpoints:
    - {name: game,  protocol: tcp, port: 25565, exposure_default: stable}
    - {name: query, protocol: udp, port: 25565}
    - {name: rcon,  protocol: tcp, port: 25575, exposure_default: overlay_only}
  volumes:
    - {name: data, path: /data, size_default: 10Gi}
  resources:
    memory: {min: 1Gi, recommended: 4Gi}
    cpu:    {min: 500m, recommended: 2000m}

config:                                     # §5
  fields: [...]

install:                                    # §4: restricted, no arbitrary code
  steps: [...]

templates:                                  # §6: sandboxed, allow-listed
  - from: templates/server.properties      # the source: a file of the artifact (§2)
    path: /data/server.properties           # where it is rendered, inside the workload
    format: properties
    vars: [motd, max_players, difficulty]

probes:                                     # Rule K-16
  readiness: {kind: query, protocol: minecraft, endpoint: query, timeout: 5s}
  liveness:  {kind: query, protocol: minecraft, endpoint: query, failures: 3, period: 30s}

logs:
  events:
    - {pattern: 'Done \(.*\)! For help', emit: ready}
    - {pattern: 'Can''t keep up!', emit: performance_warning, level: warn}

lifecycle_hooks:
  pre_stop:  {kind: console_write, data: "stop\n", wait_for_exit: 120s}

disruption:                                 # §5.1: what this software costs to interrupt
  stop:     {disruptive: true,  reason: "disconnects players"}
  restart:  {disruptive: true,  reason: "disconnects players"}
  reconfigure:
    disruptive_fields: [max_players, difficulty]   # applied only by a restart
```

`pre_stop` deserves a note: a game server sent `SIGTERM` may not flush its world to disk. Writing
`stop` to the console and waiting is the difference between a clean shutdown and a corrupted save,
and it is declared by the recipe rather than special-cased in core.

> Not yet as built, found 2026-10-17 (built 2026-10-19; see the note below). A recipe's `pre_stop`
> is read and checked (it writes something, and waits at most ten minutes) but no node runs it:
> every stop, a restart's included, sends `SIGTERM` and kills after thirty seconds. The panel's
> browser test found it, when a restart of a shell that ignores `SIGTERM` as a container's first
> process took the whole grace. Until the agent runs the hook, a recipe whose software needs it
> gets the unclean stop this section warns of. It is task P1.13c of the build plan, before Phase 1
> is declared done.

> As built, 2026-10-19, on the node (`crates/korpis-agent/src/reconcile/halt.rs`). The hook
> needed no new field: the signed assignment carries the recipe document, and the agent reads
> the hook from it once for each recipe it is assigned. Every stop of a running object, a
> workload's or a run's, whatever needed it (a power stop, a restart, a rebuild, a retirement, a
> supersession, a run's cancel, replacement, or timeout), first writes the hook's data to the
> console as `system:reconciler`, which the driver records in the stream as that input, and
> journals it as the node's `node.pre_stop` effect; the process then has `wait_for_exit` to exit
> by itself, and only then is it signalled, and killed once its grace of thirty seconds is over.
> None of it holds up the node's other workloads, since a stop is moved on pass by pass (§6 of
> `02-architecture.md`). What it does not do: a workload whose console takes no input (a
> `console: logs` or `none` workload, or one whose driver cannot write to it) cannot be asked,
> which the effect records as failed with the driver's reason, and it is signalled at once; and
> nothing shortens a stop of software that heeds neither the hook nor the signal below its
> `wait_for_exit` plus the grace. A plan says so: each step that stops a running object carries
> that sum as `stops_within_seconds` (§3.3 of `01-model.md`). Verified on a real node the same
> day: a restart of software that ignores `SIGTERM` and exits only on `stop` completes in seconds
> (`crates/korpis-e2e/tests/e2e/stopping.rs`), and Finding 87 of `23-walkthroughs.md` is resolved.

### 3.1 A declaration is completed by its recipe, once, and the intent stores the result

> Added 2026-10-13, from the implementation (`crates/korpis-core/src/workload/declare.rs`). The
> declared file of §3 of `13-surface-cli.md` leaves out the runtime, the probes, and a volume's
> path, and §4 of the same document has `korpis plan` show what the control plane will actually
> run. Nothing said who fills in what a declaration leaves out, or when.

**The control plane completes a declaration against its recipe at declaration time, and the
intent stores the completed body.** What is left out comes from the recipe resolved to its digest:

| Part | When the declaration leaves it out |
|---|---|
| `lifecycle`, `console` | the preset's; a preset that names no console asks for `logs` |
| `runtime.tier` | the recipe's first target's tier; a declared tier must be one the recipe has a target for |
| `runtime.driver` | the Linux driver for the tier in §3 of `04-runtimes.md`: `oci`, `native`, `firecracker`, `qemu` |
| `resources` | when neither reservation nor limit is declared, the preset's recommendation is reserved |
| `volumes`, `endpoints` | every preset entry is kept; a declared entry of the same name replaces what it states and inherits what it leaves out; other declared entries are added |
| `health` | each probe not declared is the recipe's |

Whatever is declared must also reach the preset's minimum resources, which is what the software
needs to start: a declaration below it is refused, not placed and left to crash. An endpoint with
no exposure, declared or proposed, is `stable`, which placement resolves to `node_bound` where the
installation cannot deliver it (§3 of `07-networking.md`).

Why at declaration and on the server: an intent is immutable and must mean the same thing forever
(§3.1 of `03-state.md`), so it cannot depend on reading its recipe again; and every client, the
CLI, the web panel and a chat command, gets the same completion because only one place performs it,
which is what lets `korpis plan` show the body that will run. Why merge entries by name rather than
replace the lists: a recipe's probes name its endpoints, and a declaration that adjusts one
endpoint's exposure should not have to restate the others to keep its readiness probe valid.

A console is a capability the node's driver must declare (§4.1 of `04-runtimes.md`), so the default
is not free: the `oci` driver declared none until the durable streams of Phase 1 (§4 of
`20-roadmap.md`), and a workload asking for `logs` was placed on no Phase 0 node. A recipe meant to
run there said `console: none`, and the scheduler's explanation names the console when a node lacks
the one asked for. Since those streams were built the `oci` driver declares `none`, `logs`, and
`interactive` (§4.2 of `04-runtimes.md`); corrected 2026-10-19, as Phase 1 closed.

What it costs: a declared list cannot remove a preset entry. A workload that must not expose a
preset endpoint declares it with exposure `none`; a preset volume cannot be dropped at all, because
the recipe's software was packaged expecting it. And a workload for a non-Linux host must name its
driver, because the default is the Linux one.

---

## 4. The install DSL

**There is no step that runs arbitrary code. No `run`, no `exec`, no `shell`, no `script`.**

| Verb | Arguments | Notes |
|---|---|---|
| `fetch` | `url`, `sha256` \| `sha512`, `to` | **the hash is mandatory**, always, no exceptions |
| `copy` | `from` (a recipe layer), `to` | content already inside the recipe |
| `extract` | `from`, `to`, `strip`, `include` | path traversal and absolute-path entries rejected |
| `mkdir` | `path`, `mode` | |
| `write` | `path`, `content`, `mode` | literal content from the recipe |
| `template` | `from`, `to`, `format`, `vars` | §6 |
| `chmod` | `path`, `mode` | |
| `chown` | `path`, `uid`, `gid` | workload-relative ids, 0 to 65535 (§4.5 of `04-runtimes.md`) |
| `symlink` | `target`, `link` | both must resolve inside the volume |
| `delete` | `path` | |
| `<publisher>/<step>` | `to`, and the provider's own | a step provider's (below); its artifacts are written beneath `to` |

`chown` first took an `owner` "within the tenant's uid range", which left open whether it named a
user, a number, or a pair, and ranges turned out to be per workload, not per tenant. It takes `uid`
and `gid` as the workload sees them, which is how files are stored on disk (§4.5 of
`04-runtimes.md`): `0` is the workload's own root, a value above 65535 does not load, and no value
can name a host identity, because the mapping is applied at mount and not by the step.

Every step that touches the volume executes through the workload's own confined filesystem worker,
in a mount namespace that holds that workload's volumes and nothing else (§6 of
`02-architecture.md`), unprivileged, Landlock-restricted, with every path resolved by
`openat2(RESOLVE_BENEATH)`. Rule K-1 applies to installation exactly as it applies to the file
manager, the installer has historically been the *more* dangerous of the two.

**The worker as built, 2026-10-15** (`korpis-install-job`). The helper starts it for an install,
as it starts a backup job (§5.5 of `06-storage.md`), and it runs as two processes. The first is
root for as long as it takes to build the second's world: it joins the job's cgroup and makes a pid
namespace, and the worker is that namespace's first process, so it can name, signal, or trace no
process outside it, and it dies with the first. The worker drops its supplementary groups and
makes its own user, mount, network, and IPC namespaces. The first maps the worker's ids, 0 to
65535, to the id range the workload itself runs as, which the helper checks against its ledger
(§4.5 of `04-runtimes.md`). It mounts each volume as an idmapped copy through the worker's user
namespace, and builds the worker's root, a read-only tmpfs holding only those volumes at the paths
the workload sees them at and the node's content, read-only, at `/.korpis/content`, with nothing of
the host in it, not even `/proc`. The worker then becomes its user namespace's root, which is the
workload's root and no one else on the host: what it writes lands on disk as the workload's ids,
and its capabilities (four: setting owners and modes on what it writes) hold only over the
workload's files. It applies Landlock to the volumes, its output directory, and reading the
content, takes no new privileges, and has no network to connect to. Its progress is written after
every action, and its result once it ends, in a directory of its own, so a worker lost part way
is resumed by a new job at the action it did not finish, and what confined it is in the result
as the kernel reported it.

Why the workload's own ids rather than root with few capabilities: the worker reads what a recipe
supplies, archives above all, and §6 of `02-architecture.md` says nothing that accepts tenant
input holds privilege. Root with four capabilities in the host's user namespace would be privilege,
however narrowed, and would share an identity with every root process on the node. As the
workload's ids it holds nothing the workload did not already have. What it costs: an install needs
the workload's id range, so a workload is installed on a node that holds its range, which is every
node it is placed on; and it needs idmapped mounts (ext4, XFS, btrfs, and ZFS from OpenZFS 2.2,
§4.5 of `04-runtimes.md`), so on a storage class without them an install fails and says why,
rather than running as anything else. What it does not do: it applies no seccomp filter, so the
kernel's whole system call surface is reachable from a compromised decoder, as it is from the
workload; and the first process is root for the moments it takes to build the root, and decodes
the spec, which carries the recipe's actions, though it acts on none of them.

**A `fetch` is performed by the node's content store, not by the worker.** The content store
downloads the URL once per node, verifies the declared hash, and keeps it by digest (§6 of
`06-storage.md`); the worker then copies it into the volume. So a second workload installing the
same file fetches nothing (K-15, which the first version applied to images and not to install
steps), and the worker that handles tenant paths never needs network access at all.

**A `fetch` reaches the public internet, and the networks the operator names, and nothing else.**
Decided 2026-10-15. The URL is the recipe's, and anyone may write a recipe, so the node's request
is a request the recipe's author chose: to the node's own loopback, to the cloud metadata service
at `169.254.169.254`, or to a service on the operator's private network. The hash protects what is
kept, not the request, and whether a fetch fails, and how, tells its author what answered. This is
Pterodactyl's SSRF class, three of its advisories, one of them in exactly this place: a remote
file pull (CVE-2024-34068, §1.1 of `research/evidence.md`). So the node connects only to an
address on the public internet unless the operator allows its network (`--fetch-allow`, §3 of
`18-operations.md`): every address a name resolves to is checked by the resolver the connection
then uses, so a name cannot pass the check and connect elsewhere; an address written in the URL
is checked before anything is sent; and every redirect is checked the same way. Loopback, private
(RFC 1918 and IPv6 unique-local), link-local, carrier-grade NAT, multicast, documentation, and
reserved ranges are refused, as is an IPv6 address carrying one of those IPv4 addresses. What it
costs: a mirror on a private network has to be named by the operator, and a recipe cannot fetch
from a server on the operator's own network without that. What it does not do: it does not stop a
fetch reaching any public address, which the recipe's author could reach anyway.

A `fetch` reads only what a fetch downloaded. The node keeps those bytes apart from the rest of
its content store, so a digest a recipe names never reaches another recipe's file or an image
layer, which may come from a tenant's private registry (§6 of `06-storage.md`, Finding 49).

**A `fetch` without a hash is not a warning. It is a parse error.** The recipe does not load.

**When an install runs.** Decided 2026-10-14, when the install was planned. An install is one plan
step, `install`, and it runs in three cases. When a workload is created, after its volumes exist
and before its instance does: every verb, then every template of the `templates` section. When its
recipe changes to another digest: the instance is stopped, the new recipe's verbs and templates
run over what the volumes hold, and it is started again. And when values that templates of the
`templates` section use change: those templates, and only those, are rendered again before the
instance is reconfigured. A workload created from a backup is not installed, because its volumes
hold the installed tree the backup was taken of. The step's detail carries why it runs and every
action it runs, in order: each verb with its arguments, and each template with its context, so the
plan shows exactly what will be written and the node that runs it needs nothing else of the
recipe; it cannot be undone, because what it writes over is not kept.

> Corrected 2026-10-15, when plans began to carry the step. An install that writes only into
> volumes its own plan creates writes over nothing, and undoing the plan removes the volumes and
> what was written in them, so that one step is reversible, and creating a workload is not made to
> wait for a confirmation it would not otherwise need. Every other install, a recipe change, a
> template rendered again, or a creation that attaches an existing volume, writes over what is
> there and stays irreversible, so its plan is confirmed (§3.3 of `01-model.md`).

**How a node runs the step, as built 2026-10-15.** The node first fetches what the actions read
into its content store: each `fetch` by its declared hash (§6 of `06-storage.md`), and the recipe
itself with its files when a `copy`, a `template`, or an `extract` of one of them reads it. It then
has the helper start the worker (below) over the volumes the workload mounts read-write, at the
paths it mounts them, and waits while it runs, as it waits for a backup. How far an install has
come is kept by the node per install, not per plan: a digest of the workload and its actions names
the install and its job. A worker that fails has its progress kept and its job forgotten, and the
step fails; a later plan carrying the same install starts at the action that failed. A worker
that is lost, killed or taken down with its node, is started again by the same step at the action
it did not finish, and three losses in a row fail the step rather than looping.

**Everything an install writes lands in a volume.** Every path a verb or a template writes must be
one of the workload's volumes or beneath one, and a declaration whose volumes leave one outside is
refused, naming the step. The worker that writes sees the workload's volumes and nothing else
(§6 of `02-architecture.md`), and a file written anywhere else would vanish with the instance; a
recipe that relied on it would install a workload that starts once. A provider step's own target
is its `to`, checked with the rest; the paths of what it answers are beneath it, and are checked
when it is resolved. Nothing a recipe names, and no volume, lies at or beneath `/.korpis`, which
is where the node puts what it gives the worker to read.

What it costs: an upgrade that should keep a file the tenant edited has to say so by not writing
it, because a verb overwrites what it targets (below); and a declaration that moves a volume can be
refused for a recipe it did not change.

### Every verb states its own idempotence

> Finding 20 of `23-walkthroughs.md`.

An install is one Plan step and several DSL steps, and §3.1 of `05-scheduling.md` guarantees
resumability at the Plan's granularity. Left there, a provider outage during step four means
re-running a forty-gigabyte fetch that had already succeeded, over a directory that the same
section deliberately retained, which is how `extract` and `template` produce a workload that starts
and is quietly wrong.

The verb list being closed is what makes the fix cheap. Each verb declares what re-running it does:

| Verb | Re-running it |
|---|---|
| `fetch` | no-op if the digest is already in the content store and at the target; otherwise refetch or recopy |
| `copy` | overwrites the target with the layer's content, never merges |
| `extract` | overwrites into the target tree, never merges into whatever was there |
| `mkdir` | no-op if the directory exists with the declared mode; corrects the mode otherwise |
| `write` | overwrites the target with the literal content |
| `template` | renders from the source template every time, never edits its own output |
| `chmod` / `chown` | sets the declared value; idempotent by definition |
| `symlink` | replaces a link at the path; refuses to replace anything that is not a link |
| `delete` | no-op if already absent |
| a provider step | re-resolves, and the worker overwrites the targets with what it returns, as for `copy` |

> Corrected 2026-10-13, when the step provider interface was built (§5 of `16-extensions.md`). The
> provider-step row said the provider declares its idempotence and one that cannot is refused at
> install. The interface made the declaration unnecessary rather than unenforced: a provider only
> resolves a step into content-addressed artifacts, and the node fetches them and the workload's
> worker writes them, so no provider can write the volume at all, and re-running a step can only
> re-resolve it and overwrite the same targets. The guarantee is now the interface's, which holds
> for every provider, where a declaration would have held only for the honest ones. What a re-run
> does not promise is the same bytes: an unpinned step may resolve to newer content, which is the
> non-reproducible mark below.

> Second external review, finding S73. The first version of this table listed `verify`, which is not
> a verb, and omitted six verbs that are, so the guarantee "each verb states its own idempotence"
> covered half the closed set it relied on.

Install progress is recorded durably per install, not per plan, so a resume continues at the verb
that failed. This is the property the escape hatch below depends on: a step provider is
third-party code on the install path, and it will be the thing that fails.

**The verbs as built, 2026-10-14** (`crates/korpis-install`). Each path is resolved beneath the
volume it lies in by `openat2` with `RESOLVE_BENEATH`, no magic links, and no crossing into another
mount, so a link the tenant planted makes a verb fail and never makes it write elsewhere. A file
(`fetch`, `copy`, `write`, `template`) is written beside its target, synced, and renamed over it,
with the directories above it made; content and templates are written with mode `0644`. `extract`
recognises tar, bare or in gzip, zstd, or xz, and zip by their first bytes; it refuses an absolute
or `..` entry, a device or FIFO, a link out of what it unpacks, and a hard link to anything not
unpacked with it, and for any of these unpacks nothing. It unpacks into a fresh tree beside its
target and swaps the two, so the target becomes what the archive holds, and it never takes a
volume's root as its target, because swapping that would delete everything the workload wrote
there. Unpacked files belong to the workload's root whatever owner the archive records; a `chown`
says otherwise. `chmod` refuses a link; `chown` changes a link's own owner, never what it points to.
`symlink` points within the link's own volume. `delete` of something absent does nothing, a link
out of the volume on the way to it is a failure rather than an absence, and a volume's root is
never deleted. What it costs: a recipe cannot unpack straight into a volume's root, and has to
name a directory beneath it.

### The escape hatch that does not reopen the hole

Real installs sometimes need something a verb list cannot express. SteamCMD is the obvious case,
and it is what half this market's games require.

The answer is not to allow scripts. **An extension registers a provider for a step kind**, and the
recipe uses it declaratively:

```yaml
install:
  steps:
    - valve/steam.app: {to: /data/server, appid: 730, branch: public, manifest: 8471…}
```

The `valve/steam.app` provider is contributed by an extension. It runs **in the extension's own
sandbox, under the extension's own grant** (§3.7 of `01-model.md`, P8), not with the installer's
privileges, not in the tenant's namespace, and not with the ambient authority of core. It resolves
the request into content-addressed artifacts, which then flow through the ordinary content store,
and the workload's filesystem worker copies them in like any other content.

`to` is the recipe's, not the provider's: the workload path, in one of its volumes, that the
step's artifacts are written beneath. It is checked at declaration like every other target, and
no provider declares an argument of that name. Decided 2026-10-15 (Finding 50 of
`23-walkthroughs.md`): the interface had said an artifact's path was relative to "the volume
root", and a workload with two volumes has two.

The step is named by publisher, the provider that answers it is pinned by digest in the lockfile,
and its contract (argument schema, idempotence, failure reasons, and whether its output was pinned
or merely current) is §5 of `16-extensions.md`. A step that names exact content, as `manifest` does
here, is reproducible. One that names a channel (`branch: public` with no manifest) gets whatever
the channel holds today, so the workload is marked non-reproducible, as an unverified egg is (§9),
and the recipe says so. "Two workloads from one digest run the same bytes" (§2) is true of
reproducible recipes, and the label says which ones those are.

The capability exists. Arbitrary code in a recipe does not. And because providers are extensions,
adding SteamCMD support requires no change to Korpis and no new core privilege.

---

## 5. Configuration is typed and permissioned

Rule K-17. This is TCAdmin's graphical configuration designer and command-line builder, features it
sells as premium, expressed as a core primitive.

```yaml
config:
  fields:
    - key: max_players
      type: integer
      min: 1
      max: 200
      default: 20
      permission: tenant
      label: "Maximum players"

    - key: motd
      type: string
      max_length: 59
      pattern: '^[^\r\n]*$'               # a single line
      permission: tenant

    - key: rcon_password
      type: string
      secret: true                         # redacted from streams (§7 of 14-streams.md)
      permission: operator
      deliver: env                         # §5.2: never into a tenant-writable file
      deliver_as: RCON_PASSWORD

    - key: java_heap
      type: quantity
      permission: operator                 # the tenant cannot set this
      derived: "resources.memory * 0.8"

    - key: jvm_flags
      type: string
      permission: operator
      pattern: '^[-A-Za-z0-9:+=./ ]*$'
      deliver: env                         # the java launcher splits this variable itself
      deliver_as: JDK_JAVA_OPTIONS

    - key: eula
      type: boolean
      const: true
      hidden: true
```

| Permission | Who may change it, as a rule the gateway evaluates |
|---|---|
| `tenant` | a holder of `workload.write` on the workload |
| `operator` | a holder of `workload.write` through a grant scoped to the **parent** of the workload's owning organization, or above it; for a root organization's own workloads, its own scope |
| `system` | Korpis only, derived or fixed |

> Second external review, finding S31. Recorded in §17 of `23-walkthroughs.md`. The permissions were
> role words ("the workload's owner", "the organization above them") with no action behind them, so
> "grant-checked like everything else" had nothing to check. Each is now an action on a scope the
> gateway can compare.

Three properties follow:

1. **Korpis generates the form.** No hand-written UI per recipe, and the form matches the
   validation exactly because both come from the same declaration.
2. **Validation is server-side and total.** A tenant cannot set an `operator` field, not hidden in
   the interface, *rejected at the API*. Field permissions are grant-checked like everything else.
3. **Invalid intents never exist.** Configuration is validated at declaration (§3.3 of
   `01-model.md`), so an invalid value is a rejected API call rather than a workload that fails to
   start twenty minutes later.

Pterodactyl offers untyped environment variables and a raw text editor. The gap between that and a
per-field permission model is most of what an operator does all day.

### 5.1 Disruption is declared by the recipe

The override chain of §3.3 of `01-model.md` (finding R9) gives the recipe a layer between the driver
and the operator: what this software costs to interrupt. That layer is the `disruption` block of §3.
It may only make a step more disruptive than the driver says, never less, and fields listed in
`disruptive_fields` make a configuration change that touches them a disruptive plan. A recipe that
declares nothing inherits the driver's classification. This is the field §3.3 of `01-model.md`
cited and this document had not defined (finding S17).

### 5.2 An operator field is enforced where the tenant cannot write

> Second external review, finding S31.

Rejecting a tenant's attempt to set `jvm_flags` at the API is worthless if the value is rendered
into `/data/server.properties` and the tenant holds `workload.file.write` on `/data`: they edit the
file instead. A permission that a file edit can override is a displayed limit nothing enforces
(K-3).

So every field declares how it reaches the software, and an `operator` or `secret` field may only
use a channel the tenant cannot write:

| `deliver` | Reaches the software as | Tenant-writable | Allowed for `operator` and `secret` |
|---|---|---|---|
| `env` | an environment variable set by the runtime | no | yes |
| `arg` | a command-line argument set by the runtime | no | yes (not for `secret`: arguments are visible in the process table) |
| `managed_file` | a file rendered into a read-only mount outside the tenant's volumes | no | yes |
| `file` | a file in the tenant's volume | **yes** | **no**: rejected when the recipe is loaded |

Software that reads a setting only from a file in its own data directory can still have that
setting typed, validated, and generated as a form, but it cannot be an `operator` field, and the
recipe loader says so rather than letting the recipe claim a protection it cannot provide.

`secret: true` marks a field whose value is redacted from streams before persistence (§7 of
`14-streams.md`), never rendered into a template the tenant can read, never shown in plans or
effects (they record that it changed, not what to), and readable back only with
`workload.secret.read` (§5 of `10-api.md`).

> Corrected 2026-10-17 (Finding 84 of `23-walkthroughs.md`). The last clause was not true of
> what was built: every read that returns an intent (`GetWorkload`, `ListWorkloads`,
> `WatchWorkload`, `ListIntents`, and the answer to `DeclareIntent`) returned its body as stored,
> so a holder of `workload.read`, a share link's among them, read the RCON password the example
> above marks secret. Now each of those withholds, from a caller without `workload.secret.read`
> on the workload, the values of the fields the intent's recipe marks secret, and names the keys
> in the intent's `withheld`, so a client knows a value is set without learning it; a recipe
> that cannot be read withholds every value. Such a caller changes everything else by declaring
> with `keep`, which carries the named values from the head as they are, so it never has to
> know a secret to leave it alone. A kept value goes wherever the recipe it is declared with
> sends it, and only the recipe it was set under is known to keep it out of the caller's sight:
> another, even one that marks the field secret, could hand it to a command that prints it to a
> console the caller reads, encoded past redaction. So keeping a value into any recipe but the
> head's, by digest, needs `workload.secret.read`, as reading it would. What it costs: one more authorization check and a recipe read
> per workload a read returns, both kept per call; and a clone made by a caller who may not
> read the source's secrets starts with the recipe's defaults for them, since a value it was
> never shown is not one it can copy.

> Note, 2026-10-12, from the implementation (`crates/korpis-core/src/recipe/config.rs`). The wire
> schema names the `file` channel `CONFIG_DELIVERY_TEMPLATE`: the field reaches the software only
> through templates rendered into its volumes, which is the same tenant-writable channel. The
> loader treats them as one, and refuses an `operator` or `secret` field that uses it.

### How a value is written for the runtime

`env` and `arg` values are set by the runtime driver when it prepares the workload (§4.2 of
`04-runtimes.md`), from the resolved configuration the intent stores (§3.1), in the schema's
declaration order.

An `env` field's `deliver_as` names the variable; two fields may not name the same one. An `arg`
field's `deliver_as` is a template of one or more arguments separated by spaces, and `{}` in an
argument is replaced by the value: `--port {}` gives two arguments, `-Xmx{}` one. **A value is
never split**: a value containing a space is still one argument, so no value can add a flag the
template did not write. A field that needs a list of flags delivers it through a variable the
software splits itself, which is why the example above sends `jvm_flags` to `JDK_JAVA_OPTIONS`. A
template must place the value, except for a boolean, whose template without `{}` is a flag:
present when the value is true, absent when it is false. A field with no value, or a null one,
sets nothing.

The value is written as text by its field's type, in one form whichever one was declared: a
string or an enum as it is, an integer in decimal, a number in its shortest exact decimal, a
boolean as `true` or `false`, a quantity in its base unit (`4Gi` is `4294967296`, `500m` is
`0.5`), and a duration in seconds (`1h30m` is `5400`). When the recipe's `entry` names no command,
the arguments replace the image's own `CMD`, as an OCI runtime does.

A `managed_file` field is rendered by Korpis rather than set by the driver. It first arrived with
the templating of Phase 1 (§4 of `20-roadmap.md`), and until it did a node refused at `Prepare` an
intent in which such a field had a value: starting the software without a value its recipe
promised it would be a quieter failure than not starting it. Corrected 2026-10-15: the node
renders them now, as below, and the refusal is gone. A node that gives its workloads no directory
of their own still refuses such a recipe, for the same reason.

**Where a managed file is, and what renders into it** (decided 2026-10-15, from the
implementation). The table above said "a file rendered into a read-only mount outside the
tenant's volumes" and nothing said which file, at which path, from what. A recipe declares its
managed files beside its templates:

```yaml
managed_files:
  - {name: limits.properties, from: managed/limits.properties, format: properties,
     vars: [max_view, rcon_token]}
```

Each is rendered by the node from a template in the recipe, with the engine and the encoding of
§6, and appears to the workload at `/run/korpis/config/<name>`, in the read-only directory the node
already gives every workload for its identity socket (§6 of `02-architecture.md`). A name is a
plain file name, unique in the recipe. A managed file's `vars` may name only `managed_file`
fields, and a `managed_file` field must be named by at least one managed file: a value with
nowhere to go is a setting the software never sees, which the loader refuses rather than accepts.
A template in a volume never names a `managed_file` field, as before. Unlike a template in a
volume, a managed file may render a secret: it is the node's file, read-only to the workload, and
in no volume a tenant reads through Korpis.

Why a fixed directory rather than a path the recipe chooses: the directory is already mounted
read-only into every workload, so a managed file needs no mount of its own, no path the driver
must check against the volumes, and no rule for a recipe naming `/etc/passwd`. What it costs:
software that reads a setting only from a fixed path elsewhere cannot use a managed file unless
it accepts the path as an argument, which the recipe's `entry` can give it, or its image links
that path to the managed one. Software that reads a setting only from its own data directory
remains outside this channel, as the paragraph after the table says.

As built: the agent renders every managed file each time it builds the workload's runtime object,
so a changed value reaches the software at its next start, as every other setting does. It reads
each template from the recipe's files in its content store, at most 1 MiB, renders it with only
that file's fields in its context, and writes it whole, mode `0444`, into `config/` of the
workload's directory, removing any file the recipe no longer declares. The agent renders rather
than the workload's filesystem worker (§6): the engine reads nothing but its source and its
context, and each file is written by name into a directory of the node's, so a template has no
path to escape by.

> Note, 2026-10-13, from the implementation (`crates/korpis-core/src/recipe/deliver.rs`). The
> schema said `deliver_as` was "the argument template" and never said what a template was, and the
> example above could not be loaded: `rcon_password` named no variable, and `jvm_flags`, an
> operator field, fell to the default `file` channel that this section forbids it. The example is
> corrected and the rules are this section.

### 5.3 Derived fields, and the language they are written in

> Added 2026-10-12. §5 showed `derived: "resources.memory * 0.8"` and no document said what an
> expression may contain, who evaluates it, or when, so the field could not be implemented.

A derived field is computed by the **control plane, at declaration**, from the intent being
declared, and the result is written into the intent like any other value. A node never evaluates
an expression, an old intent keeps the value it was declared with, and a rollback restores it.
The language is deliberately small:

```
expr    := term (('+' | '-') term)*
term    := factor (('*' | '/') factor)*
factor  := number | quantity | path | '-' factor | '(' expr ')'
         | ('min' | 'max') '(' expr ',' expr ')'
path    := 'resources.' ('cpu' | 'memory' | 'disk') ['.reservation' | '.limit'] | 'resources.pids'
         | 'config.' key
```

`resources.memory` without a suffix means the limit if one is set and the reservation otherwise;
CPU is in millicores, memory and disk in bytes. A `config.` path may name only a numeric field
that is not itself derived, so evaluation needs no ordering and cannot cycle. A derived field
must be `integer`, `number`, or `quantity`, must have permission `system`, and its result is
rounded down for the integer types and checked against the field's `min` and `max` like a
declared value. Division by zero, a non-finite result, or a negative quantity rejects the
declaration. Expressions are limited to 256 bytes and sixteen levels of nesting.

The other syntaxes the schema relies on are fixed here as well. A **quantity** is a decimal
number with an optional suffix: binary `Ki` to `Ei`, decimal `k` to `E`, or `m` for thousandths,
with no exponent form and at most three fractional digits after scaling. A **duration** is one or
more `<integer><unit>` parts with units `d`, `h`, `m`, `s`, `ms`, largest first (`1h30m`). A
field **key** is `[a-z][a-z0-9_]*`, at most 64 bytes, and a `pattern` must match the whole value.

What it costs: a recipe author who needs a conditional or a string operation cannot express it,
and has to ask for a field the operator sets instead. That is the intended trade; an expression
language that can branch is a program, and recipes contain no programs (§4).

**The strings a recipe shows, designed 2026-10-16** (question 4 of §10 of `11-surface-web.md`). A
field's `label` and the recipe's `metadata.description` are messages in the recipe's default
locale, which the document names (`metadata.locale`, `en` if absent), in ICU MessageFormat; a
recipe's `metadata.name` is an identifier and is never translated. A recipe may carry catalogues
for other locales as files `i18n/<locale>.json` in its content, each mapping a message's key to its
translation; a key is the path of the message in the document, `config.fields.<key>.label` or
`metadata.description`, so a key changes only when what it labels does. They are part of the
artifact, signed and versioned with it. The control plane reads them when it resolves the recipe
and refuses a catalogue that is not valid MessageFormat or names a key the document does not have,
so a translation cannot add a string the recipe never showed.

> As built, 2026-10-17 (`korpis-core` `recipe/messages.rs`, `korpis-recipe` `catalogue.rs`). A
> recipe's messages, its own and its catalogues', hold **no argument**: the panel formats them with
> no values, so `{players}` would reach a screen as a formatting failure rather than as anything
> the recipe meant, and is refused where the recipe loads instead. What is left of ICU
> MessageFormat is its quoting, read the way the panel's formatter reads it: `''` is an
> apostrophe, an apostrophe before a brace quotes up to the next lone apostrophe, and a brace
> outside a quotation is refused; a message is at most 4 KiB. `metadata.locale` is a BCP 47 tag
> of the plain form (`en`, `pt-BR`, `zh-Hant-TW`), and so is the name of each catalogue's file. A
> catalogue for the recipe's own locale is refused, since that locale's messages are the
> document's and two would disagree. Each value is a string, each key one the document shows, and
> a catalogue is a JSON object and nothing else. `oras` or `korpis` pushes them as files like any
> other; `korpis` refuses to push a recipe whose catalogues would not load. The store keeps them
> beside the document, and they are checked again against it whenever it is read back. A recipe
> resolved before catalogues were read keeps none, and shows its own messages. What it costs: a
> recipe that wants a count or a name inside a label cannot have one until the panel has values
> to give it, which is the first thing to change when a recipe string needs one.
>
> Served 2026-10-17 by `WorkloadService.GetWorkloadRecipe`, under `workload.read`: the recipe
> the workload's approved intent names (its head's while nothing is approved), its metadata and
> configuration schema, and every message it shows in one locale. The caller lists its languages,
> most preferred first and at most 32; the control plane picks the first the recipe speaks, by
> its exact tag or its language alone, as the panel picks its own (question 4 of §10 of
> `11-surface-web.md`), and the recipe's own locale otherwise. A message the chosen catalogue
> lacks is given in the recipe's own words, so a client is never handed a key without its text.
> The schema keeps the document's own words; the messages are what a client shows. `korpis
> workload recipe` prints them (§2 of `13-surface-cli.md`).
>
> The panel's form, built 2026-10-17 (`web/src/config`), is property 1 of §5: the workload's page
> asks for the recipe with the browser's languages and shows every field that is not `hidden`,
> labelled in the recipe's words, its quoting read and nothing else; a boolean as a checkbox, an
> enum as its choices, the rest as text the control plane reads as the field's type, and a number
> that is not one refused before anything is sent. A derived, fixed, or `system` field is shown and
> not offered. A secret is typed where it cannot be read over a shoulder, and one withheld from the
> viewer (§5.2) starts empty: left so, it is kept as it is. Saving declares a new intent from the
> head with the changed values, against the head's version, proposed so that its plan interrupts as
> much as its risk calls for (§4 of `11-surface-web.md`); a refusal that names a field marks it,
> beside the refusal's words. Which fields the viewer may change is the control plane's to decide,
> field by field. It is not shown through a share link, since each read there spends one of its
> uses.

---

## 6. Templating is sandboxed

Rule K-2, and the direct answer to GHSA-pfvc-3p5h-x7h6, where egg templating could be induced to
render the node's daemon token and Docker registry credentials into a file the tenant could read.

The root cause was that the template's evaluation context and the daemon's secret context were the
same context. So:

- **The variable set is enumerated in the recipe** (`vars: [motd, max_players, difficulty]`) and
  nothing outside that list is reachable.
- **The context is a flat, pre-resolved map**, built by the control plane from declared values
  only. There is no object graph to traverse, no parent scope, no environment, no configuration
  object.
- **The template language is logic-less**: substitution and simple conditionals. No loops over
  arbitrary collections, no attribute traversal, no function calls, no file reads, no includes.
- **Substitution is encoded for the declared `format`.** A template declares its format
  (`properties`, `yaml`, `json`, `toml`, `ini`, `env`, `xml`), and every value is encoded for it, so
  a value can never terminate the line, the string, or the element it was placed in. A field whose
  type could carry a structural character it cannot be encoded away from (a newline in a
  line-oriented format) is rejected at validation by its type, as `motd` above is by its pattern.
  Without this, a tenant-writable `motd` containing a newline would write any key it liked into
  `server.properties`, including the operator's (finding S31).
- **Rendering happens in the workload's own filesystem worker** (§6 of `02-architecture.md`), so
  even a total escape reaches only that workload's own data.

There is no ambient scope for a template to reach into, because there is no ambient scope.

**As built, 2026-10-14** (`crates/korpis-core/src/recipe/template.rs`). The language is
`{{name}}`, a section `{{#name}}…{{/name}}` rendered when the value is present and not false or
empty, its inverse `{{^name}}…{{/name}}`, and a comment `{{! … }}`; names are field keys, sections
nest at most sixteen deep, and a template is at most 1 MiB. A name outside the step's `vars` is
not in the context, so rendering it fails. The context is built by the control plane from the
intent's resolved values and travels in the plan step's detail: text as text, numbers in the
canonical form of §5 (a quantity in its base unit, a duration in seconds), booleans as booleans,
and a field with no value as unset. Each value is written as a whole token of its format:

| Format | Text is written as | Refused |
|---|---|---|
| `json` | a JSON string, quotes included | nothing |
| `yaml` | a double-quoted scalar, every line break escaped | nothing |
| `toml` | a basic string | nothing |
| `env` | a double-quoted value, `\`, `"`, `$`, and a backquote escaped, as shells and dotenv read it | a line break |
| `properties` | escaped, anything outside printable ASCII as `\uXXXX`, so ISO-8859-1 and UTF-8 readers agree | nothing |
| `ini` | as it is | a line break, NUL |
| `xml` | character data with the five entities | a character XML 1.0 has none for |

Numbers and booleans stand bare; an unset value is `null` in JSON and YAML, an empty string in
TOML and env files, and nothing in the others. The template verb of §4 declares its format as the
`templates` section does. What it costs: a template cannot contain a literal `{{`, because the
language has no escape for its own delimiters; and a value is quoted for the author, so a
template writes `"motd": {{motd}}`, never `"motd": "{{motd}}"`.

---

## 7. Trust

```
TrustPolicy                      # an organization's own, or its nearest ancestor's (§7.1)
  accepted_signers    []Signer   cosign public keys, each named; a key's identity is its fingerprint
  allow_unsigned      bool       default false, every use is logged
  allow_unverified    bool       default false, see §9

Signer
  name                string     what an intent records as having vouched
  public_key          PEM        ECDSA P-256, as `cosign generate-key-pair` writes it
  fingerprint         sha256     of the key's DER encoding
```

> Correction, 2026-10-13, from the implementation. The policy had `require_signature` beside
> `allow_unsigned`. They were one decision stated twice, and two of their four combinations had no
> meaning an evaluator could apply: requiring a signature while allowing unsigned recipes, and
> requiring none while not allowing them. `allow_unsigned` is the decision, and its default is the
> old `require_signature: true`.

- Recipes are signed with cosign; signatures live in the registry beside the artifact, in the form
  `cosign sign --key` writes: a manifest tagged `sha256-<hex>.sig` whose layers are simple-signing
  payloads naming the signed digest. A signature counts only for the digest its payload names.
- A recipe is usable when one of the organization's accepted signers has signed its digest. A
  signature by a key the policy does not accept proves nothing to it, so a recipe signed only by
  strangers is unsigned as far as that organization is concerned.
- **The default policy runs nothing.** It accepts no signer and allows nothing unsigned, because
  Korpis ships no key it asks anyone to trust (question 4 of §13 of `17-security.md`): an
  installation runs recipes once its operator has said whose it runs.
- Verification happens **before** anything is fetched or unpacked, not after.
- Registry credentials are never held by nodes. The Content component exchanges them for
  short-lived, digest-scoped pull tokens (§7 of `02-architecture.md`).
- An operator may permit unsigned recipes. It is a deliberate, logged, per-organization decision,
  and the recipe carries the fact into every workload declared from it. What vouched for a recipe
  (the accepted signer, by name, or nothing) is recorded in every intent declared from it.

### 7.1 Which policy governs, who evaluates it, and when

**An organization is governed by its own policy, else by its nearest ancestor's, else by the
default.** Setting or removing an organization's own policy needs `org.write` over it, reading the
one that governs needs `org.read`, and both are `GetTrustPolicy` and `SetTrustPolicy` on the
organization service. A removal returns the organization to inheritance. A policy is changed by
compare-and-swap (§6 of `10-api.md`) against the organization's own version, and that version keeps
counting through a removal, so a client holding a version from before it cannot overwrite what was
set after. Every change is an effect, `org.trust_policy.set`, naming each signer by name and
fingerprint.

The reasoning is that the policy protects the organization that runs the recipe. A recipe runs as
that organization's workload, inside the isolation tier it is placed in, and the host is protected
from it by that tier (§5 of `17-security.md`), not by who signed it. What a forged or tampered
recipe endangers is the workload and its data, which are the organization's. So the organization
decides, and inheritance only fills the gap for one that has not: a reseller's customer who never
thought about signing runs what the reseller runs.

The cost is that **a parent cannot impose its policy as a ceiling.** A child whose administrator
holds `org.write` over it may accept a signer, or unsigned recipes, that its parent does not.
Whether a parent may pin a floor beneath its descendants is question 7 of §10.

**The control plane evaluates it, at declaration, on the digest.** Resolution turns `name:version`
into a digest (§8), reads the governing policy from the store, and decides before anything of the
recipe is fetched. A recipe's signatures are kept in the store with it, as the recipe itself is,
so a rollback years later is decided without the registry. The registry is asked again only when
the kept signatures do not satisfy the policy, since a signature may have been added since; if it
cannot be reached and the kept ones decided, their decision stands. A dry run decides too, so a
refusal is visible before anything is written. The refusal is `recipe.untrusted` on
`body.recipe`: it is not a permission of the caller's, no grant changes it, only the policy or a
signature does. A use under `allow_unsigned` is recorded twice: in the intent, and as an effect,
`recipe.use_unsigned`, naming the recipe, its digest, and the organization.

What it does not do:

- **It is decided once per intent, at declaration.** Changing a policy does not touch an intent
  already declared: the workload keeps running what it runs, a plan proposed before the change can
  still be approved, and cancelling or rolling back to an earlier target does not decide again.
  The next declaration does. Withdrawing trust from a recipe already running is an action on its
  workloads, re-declaring or deleting them, not a side effect of the policy. Whether a policy
  change should supersede proposed plans it no longer admits is part of question 7 of §10.
- **`allow_unverified` has nothing to evaluate yet.** The grade it reads comes from egg import
  (§9), which arrives in Phase 4 (§4 of `20-roadmap.md`). Until then the field is stored and
  returned, and no recipe carries a grade it could refuse.

> Added 2026-10-13, from the implementation. §7 said only that the policy is per organization,
> and an organization without one had no stated policy: the evaluator had nothing to read for
> every organization an installation creates after its first. Inheritance from the nearest
> ancestor is the answer, for the reasons above.

---

## 8. Locking and rollout

```yaml
# korpis.lock
recipes:
  - ref: korpis.io/minecraft/paper:1.21.4
    digest: sha256:9f2c…
    resolved_at: 2026-08-07T10:12:00Z
    artifacts:
      - {url: "https://api.papermc.io/…", sha256: "a1b2…", vendored: true}
    providers:
      - {step: valve/steam.app, digest: "sha256:5d0e…"}   # §4.1 of 16-extensions.md
```

`name:version` resolves through the lockfile once, at declaration. Afterwards the `Intent` refers
to a digest and nothing else. Upstream retagging, upstream deletion, and upstream compromise all
become irrelevant to a running workload.

Updating a recipe is an ordinary `Intent` change: it produces a `Plan` showing exactly what will
change and whether it is disruptive, and updating many workloads at once is a `recipe_rollout`
`Operation` with `max_unavailable` and a maintenance window (§3 of `05-scheduling.md`). Rollback is
declaring the previous digest, not an inverse operation (P9).

---

## 9. Importing Pterodactyl eggs

There is a large existing ecosystem and ignoring it would be wasteful. But an egg's install script
is bash, and §4 exists specifically to eliminate bash.

The importer converts what is mechanically convertible (metadata, variables, the Docker image,
startup command, config file mappings, stop signal) and analyses the install script:

| Import result | Meaning |
|---|---|
| **clean** | the script reduced entirely to declared verbs. A normal recipe. |
| **hashable** | fetches were recognized but carry no hash. The importer fetches once, records the hash, and produces a clean recipe pinned to what it observed, with that stated. |
| **unverified** | arbitrary logic remains. See below. |

An `unverified` recipe is accepted only under `TrustPolicy.allow_unverified`, and it is not treated
as equivalent to a real recipe:

- its install runs in a **microVM** (`04-runtimes.md` §2), not a container
- with **no network** except hosts the importer extracted and an operator confirmed
- it is marked non-reproducible, and every workload declared from it inherits that mark
- the operator's acknowledgement is recorded as an `Effect` with their grant

This is deliberately worse to use than a real recipe, and it is still strictly safer than
Pterodactyl, where the same script runs in a container with unrestricted network and no
acknowledgement from anyone. Migration is possible; the bad property is contained, labelled, and
visible rather than inherited silently.

---

## 10. Open questions

1. **Composition.** Fifty Minecraft recipes share most of their content. Inheritance ("extends
   `minecraft/base`") is obvious and every configuration format that added it regretted it,
   overriding, ordering, and diamond dependencies. Composition by explicit inclusion of fragments
   is more verbose and far more predictable. → here
2. **Multi-workload recipes.** A recipe that declares an app *and* its database is what a user
   wants for a one-click install, and it introduces a bundle object that owns several workloads,
   with its own lifecycle, quota accounting, and deletion semantics. Possibly a separate `Bundle`
   kind rather than a recipe feature. → here
3. **How extensions register install providers. Resolved in §4.1 and §5 of `16-extensions.md`.** §4
   makes `steam.app` an extension-provided verb. The contract, how a provider declares its schema,
   what sandbox it runs in, how failures surface in the plan, whether it can be required for a
   recipe to be `clean`, belongs in the extension contract. It is now there: publisher-namespaced
   names, registration scoped to an organization, the provider pinned by digest in the lockfile, and
   pinned or unpinned output declared per call, with unpinned output marking the workload
   non-reproducible.
4. **Recipe-level dependency resolution.** Recipes currently pin exact digests, which is correct
   and means a security fix in a base image requires re-publishing every dependent recipe. A
   constraint solver would fix that and would import npm's entire problem space. The middle
   position is automated rebuild-and-republish tooling rather than runtime resolution. → here
5. **Private recipes and registry authentication. Resolved here.** An operator's proprietary
   recipes live in a private registry. Which credentials the Content component holds, how they are
   scoped per organization, and whether a tenant can supply their own registry credentials without
   the operator being able to read them was unresolved. It was routed to `17-security.md`, which
   never carried it (finding S18).

Registry credentials are organization-scoped secrets held by the Content component, usable for the
recipes declared within that organization's subtree, and never sent to a node: nodes receive the
short-lived, digest-scoped pull tokens of §7. A tenant may supply credentials for its own registry
on the same terms. What is **not** offered is a credential the operator cannot read: the operator
runs the control plane that must use it, so any such promise would be false, and §8 of
`17-security.md` already states that tenants are not protected from the operator. A tenant who
needs that property keeps the recipe in a registry the operator's control plane is given a
pull-only, revocable credential for, and revokes it when the relationship ends.

6. **Keyless signatures and the sigstore bundle.** Verification reads cosign's keyed
   signatures in the `.sig` tag form. cosign also signs keylessly, with a short-lived Fulcio
   certificate and a Rekor transparency-log entry, and newer releases can store a sigstore bundle
   through the OCI referrers API instead of a tag. Accepting either means trusting a Fulcio root
   and a Rekor key, and stating which identities (an OIDC issuer and subject) a policy accepts in
   place of a key. → here, before the signing tooling of Phase 4 (§4 of `20-roadmap.md`)
7. **A policy floor, and policy changes against pending plans.** A parent cannot keep its
   descendants from accepting what it does not (§7.1). A floor (the parent's accepted signers
   as the only ones a descendant may accept, or `allow_unsigned` forbidden beneath it) would let a
   reseller promise its customers' workloads a provenance. It costs the customer's autonomy over
   its own workloads, and it has to say what happens to a descendant's policy that the floor
   newly excludes. Separately, a plan proposed before a policy change is approved under the
   decision made at its declaration; superseding such plans on the change, or deciding again at
   approval, would close that window at the price of approval reaching the registry. → here
