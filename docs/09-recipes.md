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
      rootfs:./layers/rootfs.tar.zst       # an OCI layer, unpacked rather than run
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
  - path: /data/server.properties
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
| `template` | `from`, `to`, `vars` | §6 |
| `chmod` / `chown` | `path`, `mode` \| `owner` | within the tenant's uid range only |
| `symlink` | `target`, `link` | both must resolve inside the volume |
| `delete` | `path` | |

Every step that touches the volume executes through the workload's own confined filesystem worker,
in a mount namespace that holds that workload's volumes and nothing else (§6 of
`02-architecture.md`), unprivileged, Landlock-restricted, with every path resolved by
`openat2(RESOLVE_BENEATH)`. Rule K-1 applies to installation exactly as it applies to the file
manager, the installer has historically been the *more* dangerous of the two.

**A `fetch` is performed by the node's content store, not by the worker.** The content store
downloads the URL once per node, verifies the declared hash, and keeps it by digest (§6 of
`06-storage.md`); the worker then copies it into the volume. So a second workload installing the
same file fetches nothing (K-15, which the first version applied to images and not to install
steps), and the worker that handles tenant paths never needs network access at all.

**A `fetch` without a hash is not a warning. It is a parse error.** The recipe does not load.

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
| a provider step | the provider declares it, and a provider that cannot is refused at install |

> Second external review, finding S73. The first version of this table listed `verify`, which is not
> a verb, and omitted six verbs that are, so the guarantee "each verb states its own idempotence"
> covered half the closed set it relied on.

Install progress is recorded durably per install, not per plan, so a resume continues at the verb
that failed. This is the property the escape hatch below depends on: a step provider is third-party
code on the install path, and it will be the thing that fails.

### The escape hatch that does not reopen the hole

Real installs sometimes need something a verb list cannot express. SteamCMD is the obvious case,
and it is what half this market's games require.

The answer is not to allow scripts. **An extension registers a provider for a step kind**, and the
recipe uses it declaratively:

```yaml
install:
  steps:
    - valve/steam.app: {appid: 730, branch: public, manifest: 8471…}
```

The `valve/steam.app` provider is contributed by an extension. It runs **in the extension's own
sandbox, under the extension's own grant** (§3.7 of `01-model.md`, P8), not with the installer's
privileges, not in the tenant's namespace, and not with the ambient authority of core. It resolves
the request into content-addressed artifacts, which then flow through the ordinary content store,
and the workload's filesystem worker copies them in like any other content.

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

    - key: java_heap
      type: quantity
      permission: operator                 # the tenant cannot set this
      derived: "resources.memory * 0.8"

    - key: jvm_flags
      type: string
      permission: operator
      pattern: '^[-A-Za-z0-9:+=./ ]*$'

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

---

## 7. Trust

```
TrustPolicy                      # per organization
  require_signature   bool       default true
  accepted_signers    []KeyRef
  allow_unsigned      bool       default false, every use is logged
  allow_unverified    bool       default false, see §9
```

- Recipes are signed with cosign; signatures live in the registry beside the artifact.
- Verification happens **before** anything is fetched or unpacked, not after.
- Registry credentials are never held by nodes. The Content component exchanges them for
  short-lived, digest-scoped pull tokens (§7 of `02-architecture.md`).
- An operator may permit unsigned recipes. It is a deliberate, logged, per-organization decision,
  and the recipe carries the fact into every workload declared from it.

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
5. **Private recipes and registry authentication.** An operator's proprietary recipes live in a
   private registry. Which credentials the Content component holds, how they are scoped per
   organization, and whether a tenant can supply their own registry credentials without the
   operator being able to read them is unresolved. → `17-security.md`
