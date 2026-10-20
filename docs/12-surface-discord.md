# The Discord Client

**Status:** design **Date:** 2026-08-07 **Depends on:** [`08-identity.md`](./08-identity.md),
[`10-api.md`](./10-api.md), [`14-streams.md`](./14-streams.md) **Implements:** Bet 1, Principles
P2, P5, P6

---

> **What this document is for.** The Discord client is a **first-party extension**, not core code,
> settled in §10. Its priority is low and it arrives in Phase 4 (§4 of `20-roadmap.md`).
>
> The document is not here to argue that people want to manage servers from chat. Most do not; the
> honest split is that hosts use the panel, solo operators use the CLI, and communities running
> their own servers use chat. It is here because designing for an **account-less client whose
> authority arrives from an external role** forces the model to be right, and a design with only a
> web panel silently permits a privileged panel, which is what happened to every product in this
> market.
>
> Read it as a constraint on the model, not as a feature commitment. What must exist in core is
> `IdentityBinding`, `GrantTemplate`, attenuable grants, and the generated action surface, all of
> which OIDC, SAML, resellers, and CI need regardless of whether a chat client is ever built.

## 1. The difference

Every Pterodactyl Discord bot in existence consumes webhooks. It can tell you a server crashed. It
cannot start it, and if it can, it does so by holding a permanent admin API key and deciding for
itself who is allowed to ask, an authorization system reimplemented, badly, in a bot.

**The Korpis Discord client is a full client of the same API the web panel uses, and holds no
authority of its own.** It has no admin key, and it never holds a user's authority either. When a
user runs a command, Discord delivers a signed interaction; the client passes it to the gateway with
the command's **one action on one resource**; the gateway has the Discord identity provider verify
the interaction and say who the user is and which roles they hold, accepts that answer once, and
hands the client a ticket for that action, which the client spends on the one call it makes, and
the gateway performs it for the asserted user (§5.2 of `08-identity.md`). If the user is not
authorized, the *control plane* refuses; the bot never adjudicates.

> Second external review, finding S21. Recorded in §17 of `23-walkthroughs.md`. The first version
> had the bot exchange the user's identity for "a capability token carrying that user's grants".
> That is the one thing `16-extensions.md` §7 forbids an extension, and if it had been allowed the
> bot would have held the full authority of everyone who ever invoked it. The bot now relays
> requests; it never possesses what authorizes them.

This is Bet 1 demonstrated rather than asserted: authority without accounts. And the reason it is
possible here and nowhere else is P6: a system with roles has to provision an account for a Discord
user before it can authorize them, while a system with attenuable grants can bind an external
identity directly and issue authority that never existed as a stored role.

---

## 2. How authority arrives

Three bindings, all explicit and all visible. The two that confer authority are declared by an
operator; the third only names a person, so it is created when that person first appears:

```
guild        →  Organization        operator-declared: one guild is bound to one organization
Discord role →  GrantTemplate       operator-declared: the role, as a set subject, is issued the
                                    template's grant, and its members exercise it
Discord user →  Subject             an individual IdentityBinding, created on first authenticated
                                    use; it confers nothing by itself
```

The role mapping is the interesting one. A community already maintains its authority structure in
Discord, there is a `@moderator` role, and the people in it are the people who should be able to
restart the survival server. Korpis reads that structure rather than asking the community to
duplicate it:

```
role  @moderator   →  template "operate"
                      actions: workload.read, workload.restart, workload.console.read
                      scope:   project "community-servers"
```

Binding the role to the template issues **one** ordinary grant, to the role as a set subject (§5 of
`08-identity.md`). Members exercise it by being members; nobody receives a copy of it, and the
template is not consulted again (§4 of `08-identity.md`), so the grant attenuates the same way as
one issued by hand and appears in the same audit trail. A member who leaves the role stops being
able to exercise it as soon as the membership is next verified, and anything that member delegated
is suspended with them (§5.1 of `08-identity.md`).

**Nobody creates an account.** A moderator who has never opened the web panel can restart a server
from their phone in the channel they were already in. That is the product.

---

## 3. Discord is a third party, and this is stated plainly

§5.2 of `08-identity.md` establishes the boundary; it belongs in this document too, because this is
where the consequences land.

- A guild administrator can assign roles. If roles map to grants, **a guild administrator can grant
  Korpis authority.** That is a deliberate delegation, and the operator makes it by choosing to
  bind the guild.
- A compromised Discord account is a compromised subject. Discord's account security is now part of
  the operator's threat model.
- Discord Inc. can see command names and, depending on interaction type, arguments. A workload name
  is often a customer name.
- Discord can go down, be blocked in a jurisdiction, or change its API.

Two defaults follow, both on by default and both overridable only by explicit operator declaration:

1. **Irreversible actions are unavailable through Discord roles.** The irreversible actions are the
   ones the action vocabulary marks so (§5 of `10-api.md`), and grants to set subjects exclude them
   by default (§5.2 of `08-identity.md`). That is enforced by the gateway at attenuation, not by the
   bot hiding commands: the bot hides what a user cannot do as a courtesy, and offers a link to the
   web panel instead. A chat message is a poor place to be certain. Revoking a grant is not on that
   list; it destroys nothing that cannot be reissued, and a moderator killing a leaked link from the
   channel where it leaked is the case that most needs to be fast (finding S35).
2. **`requires_mfa` is unsatisfiable through Discord.** A grant condition requiring multi-factor
   authentication cannot be met by a Discord identity, because Korpis cannot verify Discord's MFA
   state. High-authority operations therefore route to the web panel by construction, not by policy
   text someone can forget to write.

Every operator runs their own bot application with their own token. There is no shared public
Korpis bot: a shared bot's token would be a single credential with reach into every operator's
control plane, and no amount of care makes that a good idea.

---

## 4. Discord's interaction model happens to fit the Plan

P5 requires that every change produce an inspectable Plan before it produces an Effect. In a web UI
that tension has to be managed (§4 of `11-surface-web.md`). In Discord it is native, because
Discord's interaction primitive is already *message, then buttons, then result*:

```
/workload restart mc-survival

  ┌──────────────────────────────────────────────┐
  │ Plan · restart mc-survival                   │
  │                                              │
  │ stop, then start on node fra-3               │
  │ 14 players currently connected               │
  │ estimated unavailable: ~40s                  │
  │                                              │
  │ authorized by your @moderator role           │
  │                                              │
  │  [ Apply ]  [ Cancel ]                       │
  └──────────────────────────────────────────────┘
```

The button carries the Plan's identifier, so applying it applies *that* Plan, not a freshly computed
one that may have changed in between. The card names the authority in the words a member recognizes
(their role); the grant identifier is one click away for whoever needs to audit it, and pressing
**Apply** is a new interaction from whoever pressed it, authorized as that person. If the world
moved underneath it, applying returns `CONFLICT` (§6 of `10-api.md`) and the bot shows the new Plan
rather than doing something the user did not read.

Low-risk actions skip the button and post the Plan alongside the result, exactly as in §4 of
`11-surface-web.md`. The gradation is the same because it is the same rule, not a Discord-specific
policy.

---

## 5. Ephemeral by default

A channel is a broadcast surface. A response containing a workload's address, its logs, its
configuration, or its resource usage is a disclosure to everyone in the channel and everyone who
scrolls back through it later.

**Every response containing state is ephemeral unless the invoking user explicitly makes it
public.** Not the other way around. The failure mode of the opposite default, a support channel
where someone runs a status command and posts a customer's IP to two hundred people, is not
hypothetical; it is what happens on the first day.

`Explanation` objects, quota figures, and node identities are filtered per viewer (§8 of
`08-identity.md`) before they reach a message, so even a deliberately public response cannot
contain what its author was not entitled to see.

---

## 6. Console in chat

A console is a stream. A Discord channel is not. Piping one into the other produces an unusable
channel and a rate-limited bot, which is why nobody does it, and the reason it looks impossible is
that everyone assumes chat means one message per line.

**A console session is a single message, edited in place, holding a rolling window.**

```
/console mc-survival

  ┌──────────────────────────────────────────────┐
  │ mc-survival · live                    ⟳ 2s   │
  │                                              │
  │ [14:32:19] Player Steve joined the game      │
  │ [14:32:41] Saved the game                    │
  │ [14:33:02] Player Alex left the game         │
  │                                              │
  │  [ Send command ]  [ Pause ]  [ Open full ]  │
  └──────────────────────────────────────────────┘
```

- Edits are coalesced to Discord's rate limits, so a workload printing a thousand lines a second
  produces a message updated a few times a second showing the tail, with a **visible marker for
  what was skipped**, never a silent omission (§7 of `10-api.md`).
- **Send command** opens a modal. Input is never dropped or reordered, and requires
  `workload.console.write` separately from read.
- **Open full** is a button, not a link. Pressing it is a new interaction from whoever pressed it,
  and the bot answers that person alone (an ephemeral reply) with a link minted under **their**
  authority, for this workload and a short lifetime. A link with a token in it never appears in the
  shared message, because a bearer token in a channel is a grant to everyone who scrolls back (§6.1
  of `08-identity.md`, finding S29). The chat window is for glancing; the panel is for reading.
- The session ends on its own. Discord interaction tokens expire after fifteen minutes, so a live
  session is renewed by an explicit control rather than held open indefinitely, which is also the
  behaviour you want, since an abandoned live console in a channel is a leak with a long tail.

Scrollback beyond the window is not Discord's job. The stream is durable and seekable
(`14-streams.md`); the message is a view onto it.

---

## 7. Notifications, scoped properly

The bot does deliver events, it simply is not *only* that. The design question everyone gets wrong
is whose authority governs what a channel receives.

**A channel is a subject.** It holds its own grant, issued by someone who had the authority to
issue it, and it receives exactly the events that grant permits. The alternative, events scoped to
the authority of whoever configured the subscription, means a subscription keeps leaking after that
person's access is revoked, which is the standard behaviour and is wrong.

Channel grants are read-only by construction: a channel can receive events and nothing else, and
the chat client exercises a channel's grant only for the channels bound to its own installation. A
channel cannot be asserted as an actor by an interaction, because a channel does not press buttons;
people do, and they act as themselves.

```
#ops-alerts  subject sub_c4d1
             grant: event.read on project "community-servers"
             filter: severity >= warning
```

Revoking the channel's grant stops the channel's notifications. Auditing what a channel can see is
reading one grant.

---

## 8. The command surface is generated

Slash commands, their arguments, their types, their autocomplete sources, and their permission
hints are generated from the API schema (§1 of `10-api.md`). Hand-written command definitions drift
from the API within one release, and the drift is invisible until someone needs the thing that was
never wired up.

Autocomplete resolves against what the invoking user can actually read, which makes it a genuinely
better affordance than the web panel's search: typing `/console mc` in a guild offers exactly the
workloads that user may attach to, and no others, because existence follows read authority (§8 of
`10-api.md`), even a typo cannot reveal a name.

---

## 9. When Discord is unavailable

Nothing in the data plane depends on it. The control plane does not store state in Discord and does
not wait on it; the one thing that calls Discord is the Discord identity provider, re-verifying
memberships (§5.1 of `08-identity.md`). A Discord outage removes a surface and changes nothing about
whether workloads run, converge, or are reachable, which is the property the architecture was
chosen for, not one added afterwards.

Grants obtained through role mappings follow the one rule of §5.1 of `08-identity.md`: while Discord
cannot be reached they stay valid for the binding's declared grace after the re-verification
interval, and then they are suspended, not revoked, and resume when verification succeeds. An outage
therefore degrades to a locked door after a stated time, not to an open one indefinitely.

> Second external review, finding S22. Recorded in §17 of `23-walkthroughs.md`. This section said
> role-derived grants "remain valid until their own expiry" during an outage, described their
> lifetime as "minutes", and said the control plane does not call Discord. `08-identity.md` §5.1
> suspends them instead (finding 19), its own examples run for seven to ninety days, and
> re-verification has to call something. The failure mode here would have been a membership that
> ended during an outage continuing to authorize for as long as the grant said.

---

## 10. Open questions

1. **Chat as core or as extension. Resolved: extension.** The Discord client is a first-party
   extension registering actions in the `ext.` namespace, subscribing to events, and reading
   streams under its own grants, everything §5 of `16-extensions.md` already provides. It ships in
   Phase 4.

P8 requires this: if the extension contract cannot express a full client, the contract is broken,
and proving that it can is worth more than the convenience of building it in-core. Nothing about
Bet 1 depends on where the code lives, the bet is about authority flowing without accounts, and
that machinery (`IdentityBinding`, `GrantTemplate`, attenuable grants, capability tokens) is core
regardless, because OIDC, SAML, resellers, and CI all consume it.

A **chat-adapter provider interface** is defined alongside it, with Discord as the reference
implementation, so Matrix, Slack, and Telegram are ordinary third-party extensions and Korpis never
acquires a second chat-shaped codebase. The one thing that interface must carry, and the reason it
is not trivially portable, is the widget vocabulary of §4 and §6, plan-with-buttons and an
edited-in-place rolling console. Platforms that cannot express those degrade to command-and- link,
and say so.
2. **Long-poll versus gateway for event delivery. Resolved here.** Carried from §12 of `10-api.md`.
   A bot serving many guilds does not want a control-plane stream per guild. It does not need one:
   the client is an extension and holds **one** event subscription through the extension contract
   (§5 of `16-extensions.md`), filtered by the channel grants of every guild bound to it. Commands
   arrive separately, as signed HTTP interactions (§5.2 of `08-identity.md`).
3. **Guild-to-organization cardinality.** One guild to one organization is the clean model, but a
   host running support for many customers in one guild wants many organizations in one guild,
   scoped by channel. That is expressible with per-channel bindings and may not be worth the
   confusion. → here
4. **Voice-adjacent affordances.** A moderator in a voice channel during an incident is a real
   scenario; whether that means anything beyond ordinary commands is unexplored. → here
5. **Thread-per-incident. Resolved here: an extension, needing nothing new.** Automatically opening
   a thread when a workload becomes unhealthy and posting convergence progress into it is attractive
   and could equally be an extension rather than core behaviour. It was routed to
   `16-extensions.md`, which never carried it (finding S18). It is an event consumer subscribed to
   health events, writing through the chat client's channel grants, which is exactly what §5 of
   `16-extensions.md` already provides; if building it needs a core change, that is a defect in the
   contract (P8).
