# The First-Party Extension Set

**Status:** design **Date:** 2026-08-07 **Depends on:** [`16-extensions.md`](./16-extensions.md),
[`09-recipes.md`](./09-recipes.md), [`07-networking.md`](./07-networking.md) **Implements:**
Principle P8; Rules K-6, K-16

---

## 1. What this document is, and what it is not

§2 of `00-overview.md` says no game appears in the core, and that rule is not being relaxed here.
This document is about what that rule *permits*, and it is also the proof that P8 is satisfied: if
the extension interface can express everything below, it is sufficient, and if it cannot, the
interface is wrong (§2 of `16-extensions.md`).

Everything here ships as a **recipe or an extension**, built through the same mechanism a third
party would use, holding the same declared grants, running as an ordinary workload.

**Positioning, stated once.** Korpis is general-purpose underneath and must be unmistakably
excellent at game hosting on day one. These are not in tension. Generality here is structural
(orthogonal fields on a workload (§2 of `01-model.md`), not a feature list) so it costs nothing at
build time. What costs something is being nobody's first choice, and the answer to that is a
first-party set good enough that a community running game servers has no reason to look elsewhere.
The generality sells itself later, to people who arrived for something else.

---

## 2. The set

| | Kind | Extension point |
|---|---|---|
| Game query: Source A2S, Minecraft SLP, GameSpy | extension | health provider |
| RCON and non-stdout consoles | extension | console provider |
| SteamCMD application install | extension | recipe step provider |
| Minecraft handshake router | extension | edge router provider |
| Address routers: a BGP speaker, and providers' floating addresses | extension | address router provider |
| Mod and plugin sources: Modrinth, CurseForge, Paper/Spigot | extension | recipe step provider |
| Hibernation and wake-on-connect | extension | health consumer + edge hook |
| Core recipes: Minecraft (Paper/Fabric/Vanilla), Source engine, Rust, ARK, Valheim, Palworld, Terraria | recipes | none |
| DNS providers, backup targets, OIDC/SAML | extension | provider interfaces |
| Key management for tenant custody: OpenBao and Vault Transit | extension | key management provider |
| Discord client, and the chat-adapter interface | extension | actions + events + streams |

**As built, 2026-10-14.** The first of the set is the backup target for S3-compatible stores,
`korpis-target-s3` (§5.6 of `06-storage.md`). It depends on the extension SDK and the wire contract
alone, and `scripts/check-layers.sh` fails the build if it ever links anything else of ours, which
is what keeps "built through the same mechanism a third party would use" true of it rather than
merely intended. The second is `korpis-kms-openbao`, the key management provider for tenant custody
(§5.5 of `06-storage.md`): a tenant's data keys made by its own OpenBao or Vault Transit engine
(`datakey/plaintext`) and opened by it (`decrypt`), each with its `<repository>/<workload>`
context, refusing a key that is not `derived`, since such a key ignores the context and a
credential taken from the provider would open every workload's keys. It is held to the same rule,
and tested against a real OpenBao run as a host process. The third, added 2026-10-15, is the backup
target for a disk, `korpis-target-fs` (§5.6 of `06-storage.md`): its own volume served as the S3
subset a job uses, over TLS with a certificate every reader pins, and with access it mints per job,
scoped as the S3 target's is; it is held to the same rule, and is the first of the set run in a
container on a real node in its tests. The fourth, added 2026-10-20, is the OpenID Connect
identity provider, `korpis-idp-oidc` (§5.3 and §9.7 of `08-identity.md`): people sent to their
issuer with PKCE, their ID tokens validated by OIDC Core §3.1.3.7, and their groups re-verified
with refresh tokens kept in its own volume, one person at a time. It is held to the same rule,
and tested against an independent issuer (§9 of `21-stack.md`).

Four of these are worth explaining, because they are the ones our architecture makes possible and
the incumbents structurally cannot match.

---

## 3. Health means the game answers, not that the process exists

K-16 in practice. A Minecraft server that has deadlocked its main thread is a running Java process
with an open port that accepts TCP and never replies. Pterodactyl reports it as online
(permanently, until someone complains) because process liveness is what it can observe.

The query extension makes health real:

```
health:
  provider: query.minecraft
  interval: 15s
  unhealthy_after: 3 failures
```

A healthy Minecraft server answers a server-list ping. One that does not is unhealthy, and
unhealthy is an observation the whole system already knows what to do with, it raises an event (§5
of `15-observability.md`), it appears in the panel as unhealthy rather than as a green dot, and a
restart policy can act on it.

The same provider returns the player count, the map, and the version, which is what makes §6
possible and what a host wants on a dashboard anyway.

> Found 2026-10-23 not to have been built (Finding 151 of `23-walkthroughs.md`), and built that
> day: nodes run `tcp`, `http`, and `query` probes (§5 of `04-runtimes.md`), the query directed by
> the protocol's health provider, which never reaches the server itself (§5 of
> `16-extensions.md`). The Minecraft query extension is Phase 4's (§4 of `20-roadmap.md`); until
> one is installed a Minecraft server cannot be declared with a query probe, since its protocol
> names no provider. The block above is not the syntax a recipe
> declares either: a recipe names the probe and the protocol, as `liveness: {kind: query, protocol:
> korpis/minecraft, endpoint: query, failures: 3, period: 15s}` (§3 of `09-recipes.md`), and the
> protocol, pinned at declaration, is what chooses the provider (§5 of `16-extensions.md`).

---

## 4. SteamCMD, without reopening the hole

Most dedicated servers are distributed through SteamCMD, so an install step for it is the
difference between supporting a handful of games and supporting most of them.

§4 of `09-recipes.md` forbids arbitrary code in the install DSL, and a `fetch` without a hash is a
parse error. SteamCMD is exactly the thing that cannot satisfy that, it resolves an app ID to
whatever Valve currently ships. So it takes the designed escape hatch rather than a exception:

```
steps:
  - valve/steam.app:
      appid: 896660
      branch: public
      manifest: 7346914270000188842       # pins the exact depot content
```

`valve/steam.app` is a step provider registered by an extension (§4.1 of `16-extensions.md`). The
extension runs in its own sandbox, under its own grant, with declared egress to Steam's content
servers and nowhere else. The recipe does not gain the ability to run arbitrary code; it gains the
ability to ask a specific, audited extension to do one specific thing.

The resulting content is recorded by digest in the node's content store (§6 of `06-storage.md`), so
the *second* node installing the same app version fetches nothing (K-15).

**Pinned or not, and the recipe says which.** With `manifest`, the request names the content
exactly and the install is reproducible: the same recipe digest produces the same bytes. Without it,
`branch: public` means whatever Valve ships today, which is useful for a server that should track
updates and is not reproducible, so the provider reports the result as unpinned and the workload
carries the non-reproducible mark (§4 of `09-recipes.md`). The first version of this example omitted
`manifest` and said only that the content was "knowable after the fact even though it was not
knowable before", which contradicted the recipe document's promise that one digest runs the same
bytes permanently (finding S26).

---

## 5. One IPv4 address, hundreds of Minecraft servers

§4 of `07-networking.md` makes IPv6 the default and IPv4 the scarce resource, and §3.3 notes that
Minecraft Java sends the hostname it was asked for in its handshake, plus supports `SRV` records.

The handshake router reads that and routes by name on a single address and port:

```
play.example.com   ─┐
smp.example.com    ─┼─▶  203.0.113.10:25565  ─▶  the right workload, wherever it is
creative.example…  ─┘
```

This is what `ingress` does for HTTP (§3.2 of `07-networking.md`), applied to a protocol that is
not HTTP, and it is an extension, so the parser lives outside core exactly as the rule requires.

For a host, the arithmetic is direct: IPv4 is the per-customer cost that does not fall, and this
removes it for the most-hosted game in the market. The same interface takes further protocol
routers without core changes; Minecraft is simply the one worth building first.

---

## 6. Hibernation, and waking on connection

This is the one that does not exist anywhere in this market, and it falls out of pieces that are
already in the design rather than needing new ones.

A community Minecraft server is empty most of the time. It holds 4 GB of reserved memory to be
empty. A host running five hundred of them is buying memory for four hundred empty servers.

```
idle:
  after: 20m at zero players        player count comes from §3's query provider
  action: hibernate                 stop the workload, keep the volume, keep the address
  wake: on connection               the edge starts it when someone connects
```

Every part of this already exists:

- **Player count** is an observation from the query extension (§3).
- **Stopping** is an ordinary power change producing an ordinary Plan (§3.3 of `01-model.md`); the
  intent chain is untouched, which is what finding R7 was for. The first version called it "an
  ordinary intent change" (finding S2).
- **The address survives** because `stable` endpoints live on the edge, not on the node (§3.1 of
  `07-networking.md`), the entire reason that mode exists.
- **The edge sees the connection** to an address whose workload is hibernated, and requests that it
  be started, under an authority the next paragraph defines.

**Who wakes it, and what may.**

> Second external review, finding S36. Recorded in §17 of `23-walkthroughs.md`.

"The edge asks the control plane to start it" was a power change with no subject and no grant, and
any connection could trigger it, including the internet-wide scanners that probe every Minecraft
port continuously, which would wake every hibernated server every few minutes and erase the density
the feature exists for. So:

- **The authority is the hibernation policy itself.** Whoever declared the `idle` block (holding
  `workload.write`) authorized both halves of it. When the policy is applied, the edge receives a
  grant limited to `workload.start` on that one workload, conditioned on the workload being
  `hibernated`, and it expires when the policy is removed. The wake is a power plan under that
  grant, and its effect names the policy, the declarer, and the connection that triggered it.
- **A connection wakes a workload only by attempting to join.** For protocols the router
  understands, a status ping is answered by the router from the last known status and never wakes
  anything; only a login attempt does. A port scan is neither.
- **Wakes are rate-limited per workload**, with a cooldown after each wake and a daily budget
  declared in the policy, so a determined client can cost a host at most the budget, visibly.
- **Waking needs the control plane**, because it is a plan. During a control-plane outage a
  hibernated workload stays hibernated, and the router's status reply says so rather than promising
  a start that cannot happen.

**Stated honestly, per P4, because this is where a feature like this usually lies.** A cold start
takes tens of seconds and the connecting player's client will time out before it finishes. So:

- The workload's state is `hibernated`. It is never displayed as running.
- For protocols with a handshake the router understands (Minecraft first), the router answers the
  server-list ping with an accurate "starting, ~30s" while the workload boots, so the player sees a
  real status instead of a failure. That is the protocol-aware router of §5 doing double duty.
- For everything else, the first connection fails and the second succeeds, and the recipe says so.
  A workload whose protocol cannot tolerate that declares itself ineligible; hibernation is not
  offered where it would silently degrade.
- Memory reservation is released while hibernated, so the density is real and metered as such (§2
  of `15-observability.md`). A host overcommitting on the strength of it is making a measured bet,
  not an assumed one.

---

## 7. Everything else games need is already an ordinary Korpis object

Worth listing, because the instinct is to build features for each of these and none is needed:

| Game operation | What it actually is |
|---|---|
| scheduled restarts | a `scheduled` workload with a `RunPolicy` (§4 of `05-scheduling.md`) |
| Rust wipes, map rotation | the same |
| back up before updating | an `Operation` with ordered steps (§3 of `05-scheduling.md`) |
| restore yesterday's world, keep today's plugins | single-file restore into a new volume (§5.4 of `06-storage.md`) |
| whitelist, ban, kick from Discord | RCON extension actions in the `ext.` namespace, which appear as slash commands automatically (§5 of `16-extensions.md`) |
| give a moderator restart rights, nothing else | a grant (§3 of `08-identity.md`) |
| let a player watch the console for an hour | a share link (§8 of `14-streams.md`) |
| move a busy server to another machine | migration with a `stable` endpoint, players reconnect to the same address |

That last row is the demonstration worth leading with. A Minecraft server with players on it moves
between physical machines and nobody edits their server list. No competitor can do it, and the
reason is not effort, it is that `(ip, port)` is load-bearing in all of them.

---

## 8. Still not in core

Nothing above changes §2 of `00-overview.md`. Core contains no game, no query protocol, no RCON, no
Steam, no Minecraft parser. If any of these turns out to require a core change, that is a defect in
the extension interface and the interface gets fixed, the exception is not granted (§2 of
`16-extensions.md`).

The test is simple and should be run: delete every extension in §2 and Korpis still runs web
services, databases, jobs, and virtual machines, with nothing missing but the games.

---

## 9. Open questions

1. **Wake-on-connect for UDP.** Source-engine games and most others are UDP, where there is no
   connection to hold and no handshake to answer with a status. Hibernation may simply be limited
   to protocols with a usable handshake, which is an honest limit but a narrower feature than it
   first appears. → here
2. **Hibernation and storage. Carried by §9 of `06-storage.md`, question 6.** A hibernated workload
   keeps its volume, so it still consumes disk and still counts against quota. Whether hibernated
   volumes migrate to cheaper storage automatically is attractive and interacts with §7 of
   `06-storage.md`.
3. **Mod source trust.** Modrinth and CurseForge are content sources whose artifacts are neither
   signed nor stable, which is the same problem as an unverified egg. The grading of §9 of
   `09-recipes.md` probably applies unchanged, and that should be verified rather than assumed. →
   `09-recipes.md`
4. **Which recipes are first-party. Carried by §10 of `19-governance.md`, question 2.** A
   first-party set implies maintenance, and §10 of `19-governance.md` already asks who decides what
   enters it. The list in §2 is a starting point, not a commitment.
5. **Query provider and cardinality.** Player counts are per-workload and fine; player *names* are
   tenant-controlled and must never become metric labels (§3 of `15-observability.md`). The query
   extension is the most likely place to get this wrong. → here
