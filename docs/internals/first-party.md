# First-party extensions

What ships beside the core as recipes and extensions, built through the mechanism a third party
uses, holding the same declared grants, running as ordinary workloads. No game is in the core; this
page is what that rule permits, and the proof that extensions are peers: if the contract cannot
express what is below, the contract is wrong and gets fixed ([extensions](extensions.md)).

Korpis is general-purpose underneath and has to be excellent at game hosting from the start. These
are not in tension: generality is structural (orthogonal fields on a workload, not a feature list)
and costs nothing to build; what costs something is being nobody's first choice, which a first-party
set good enough for a community running game servers answers.

## Contents

- [The set](#the-set)
- [Built](#built)
- [Health means the game answers](#health-means-the-game-answers)
- [SteamCMD without reopening the hole](#steamcmd-without-reopening-the-hole)
- [One IPv4 address, many Minecraft servers](#one-ipv4-address-many-minecraft-servers)
- [Hibernation and waking on connection](#hibernation-and-waking-on-connection)
- [Game operations are ordinary objects](#game-operations-are-ordinary-objects)
- [Still not in core](#still-not-in-core)

## The set

| | Kind | Extension point |
|---|---|---|
| Game query: Source A2S, Minecraft SLP, GameSpy | extension | health provider |
| RCON and non-stdout consoles | extension | console provider |
| SteamCMD application install | extension | recipe step provider |
| Minecraft handshake router | extension | edge router provider |
| Address routers: a BGP speaker, providers' floating addresses | extension | address router provider |
| Mod and plugin sources: Modrinth, CurseForge, Paper and Spigot | extension | recipe step provider |
| Hibernation and wake on connection | extension | health consumer and edge hook |
| Recipes: Minecraft (Paper, Fabric, Vanilla), Source engine, Rust, ARK, Valheim, Palworld, Terraria | recipes | |
| DNS providers, backup targets, OIDC and SAML | extension | provider interfaces |
| Key management for tenant custody: OpenBao and Vault Transit | extension | key management provider |
| Discord client and the chat adapter interface | extension | actions, events and streams ([chat](chat.md)) |

A first-party recipe has a named maintainer, so the set never outgrows what the maintainers keep
current, and one whose maintainer steps away is relabelled community rather than left to rot under
the project's name ([GOVERNANCE.md](../../GOVERNANCE.md)). The list is a starting point, not a
commitment.

## Built

Each depends on the extension SDK and the wire contract alone, and `scripts/check-layers.sh` fails
the build if one links anything else of Korpis's, which keeps "built as a third party would" true
rather than intended.

- **`korpis-target-s3`**, the backup target for S3-compatible stores ([backup](backup.md)).
- **`korpis-target-fs`**, the backup target for a disk: its own volume served as the S3 subset a job
  uses, over TLS with a certificate every reader pins, with access minted per job and scoped as the
  S3 target's. It is run in a container on a real node in its tests.
- **`korpis-kms-openbao`**, key management for tenant custody: a tenant's data keys made by its own
  OpenBao or Vault Transit engine (`datakey/plaintext`) and opened by it (`decrypt`), each with its
  `<repository>/<workload>` context. It refuses a key that is not `derived`, since such a key
  ignores the context and a credential taken from the provider would open every workload's keys.
  Tested against a real OpenBao run as a host process.
- **`korpis-idp-oidc`**, the OpenID Connect identity provider ([external
  identity](external-identity.md)): people sent to their issuer with PKCE, ID tokens validated as
  OpenID Connect Core 3.1.3.7 requires, groups re-verified with refresh tokens kept in its own
  volume, one person at a time. Tested against an independent issuer ([stack](stack.md)).

## Health means the game answers

A Minecraft server with a deadlocked main thread is a running Java process with an open port that
accepts TCP and never replies; a panel observing process liveness reports it online until someone
complains. A recipe names a query probe and its protocol:

```yaml
liveness: { kind: query, protocol: korpis/minecraft, endpoint: query, failures: 3, period: 15s }
```

The protocol, pinned at declaration, chooses the health provider, and the node runs the exchange the
provider directs; the provider never reaches the server itself ([health](health.md)). A server that
does not answer a server-list ping is unhealthy, an observation the system already acts on: an
event, the panel showing it, a restart. The same provider returns the player count, map and version.
Until a Minecraft query extension is installed, a Minecraft server cannot be declared with a query
probe, since its protocol names no provider.

Player counts are per workload and bounded; player names are tenant-controlled and never become
metric labels ([observability](observability.md)). The query extension is the most likely place to
get that wrong.

## SteamCMD without reopening the hole

Most dedicated servers ship through SteamCMD, which resolves an app ID to whatever Valve currently
ships, while the install language forbids arbitrary code and refuses a `fetch` without a hash
([recipes](recipes.md)). SteamCMD takes the designed escape, a step provider:

```yaml
steps:
  - valve/steam.app:
      appid: 896660
      branch: public
      manifest: 7346914270000188842     # pins the exact depot content
```

The extension runs in its own sandbox, under its own grant, with declared egress to Steam's content
servers alone. The recipe does not gain arbitrary code; it asks one audited extension to do one
thing. The content is recorded by digest in the node's content store, so a second node installing
the same version fetches nothing. With `manifest` the install is reproducible: one recipe digest,
the same bytes. Without it, `branch: public` tracks what Valve ships today, so the provider reports
the result unpinned and the workload carries the non-reproducible mark.

Modrinth and CurseForge artifacts are neither signed nor stable, the same problem as an unverified
package; whether recipe trust grading applies to them unchanged is to be verified, not assumed.

## One IPv4 address, many Minecraft servers

IPv4 is the scarce resource ([addresses](addresses.md)). Minecraft Java sends the hostname it was
asked for in its handshake, so the handshake router routes by name on one address and port:

```
play.example.com      -+
smp.example.com       -+->  203.0.113.10:25565  ->  the right workload, wherever it runs
creative.example.com  -+
```

This is what `ingress` does for HTTP ([ingress](ingress.md)), applied to a protocol that is not
HTTP, with the parser outside the core. IPv4 is the per-customer cost that does not fall, and this
removes it for the most-hosted game; the same interface takes further protocol routers with no core
change.

## Hibernation and waking on connection

A community Minecraft server is empty most of the time while reserving 4 GB to be empty. Hibernation
stops it, keeps its volume and address, and starts it when someone connects:

```yaml
idle:
  after: 20m at zero players     # from the query provider
  action: hibernate              # stop, keep the volume, keep the address
  wake: on connection            # the edge starts it when someone joins
```

Every part already exists: the player count is the query provider's observation; stopping is an
ordinary power change producing an ordinary plan; the address survives because `stable` endpoints
live on the [edge](edges.md), not the node; and the edge sees the connection.

**Who wakes it, and what may.** A wake with no subject and no grant would let the internet-wide
scanners probing every Minecraft port wake every hibernated server every few minutes, erasing the
density the feature exists for. So:

- **The authority is the policy.** Whoever declared the `idle` block (holding `workload.write`)
  authorized both halves. Applying the policy issues the edge a grant of `workload.start` on that
  one workload, conditioned on it being `hibernated`, expiring when the policy is removed. A wake is
  a power plan under that grant, and its effect names the policy, the declarer and the connection.
- **Only an attempt to join wakes.** A status ping is answered by the router from the last known
  status and wakes nothing; a port scan is neither.
- **Wakes are rate-limited per workload**, with a cooldown and a daily budget in the policy, so a
  determined client costs a host at most the budget, visibly.
- **Waking needs the control plane**, because it is a plan; during an outage a hibernated workload
  stays hibernated and the router's status reply says so.

**Stated honestly.** A cold start takes tens of seconds and the player's client times out first. The
workload's state is `hibernated` and never shown as running. Where the router understands the
handshake (Minecraft first), it answers the server-list ping with an accurate "starting, ~30s" while
the workload boots. Elsewhere the first connection fails and the second succeeds, and the recipe
says so; a protocol that cannot tolerate that declares itself ineligible. UDP games (most
Source-engine titles) have no connection to hold and no handshake to answer, so hibernation may be
limited to protocols with a usable handshake. The memory reservation is released while hibernated,
so the density is real and metered; the volume is kept, so it still uses disk and counts against
quota.

## Game operations are ordinary objects

| Game operation | What it is |
|---|---|
| scheduled restarts, Rust wipes, map rotation | a `scheduled` workload ([runs](runs.md)) |
| back up before updating | an operation with ordered steps ([plans](plans.md)) |
| restore yesterday's world, keep today's plugins | a single-path restore into a new directory ([backup](backup.md)) |
| whitelist, ban, kick from chat | RCON extension actions in `ext.`, which become chat commands |
| give a moderator restart rights, nothing else | a grant ([grants](grants.md)) |
| let a player watch the console for an hour | a share link ([streams](streams.md)) |
| move a busy server to another machine | [migration](migration.md) with a `stable` endpoint; players reconnect to the same address |

The last row is the one worth leading with: a Minecraft server with players on it moves between
machines and nobody edits their server list, which is impossible wherever `(ip, port)` is
load-bearing.

## Still not in core

The core contains no game, query protocol, RCON, Steam or Minecraft parser. If one of these turns
out to need a core change, the extension contract is fixed rather than an exception granted. The
test is to delete every extension above: Korpis still runs web services, databases, jobs and virtual
machines, with nothing missing but the games.
