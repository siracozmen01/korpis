# Chat

The chat client: a full client of the API, delivered as a first-party extension, holding no
authority of its own. Discord is the reference adapter. Most people do not want to manage servers
from chat: hosts use the panel, solo operators the CLI, and communities running their own servers
use chat. It is designed anyway, because an account-less client whose authority arrives from an
external role forces the model to be right, and a design with only a web panel silently permits a
privileged panel, which is what happened to every product in this market. It is not built yet.

## Contents

- [A client, not a bot with a key](#a-client-not-a-bot-with-a-key)
- [How authority arrives](#how-authority-arrives)
- [Discord is a third party](#discord-is-a-third-party)
- [Plans as messages with buttons](#plans-as-messages-with-buttons)
- [Ephemeral by default](#ephemeral-by-default)
- [Console in chat](#console-in-chat)
- [Notifications](#notifications)
- [Commands are generated](#commands-are-generated)
- [When the chat platform is down](#when-the-chat-platform-is-down)
- [An extension, and other platforms](#an-extension-and-other-platforms)

## A client, not a bot with a key

Every Pterodactyl Discord bot consumes webhooks: it can say a server crashed and cannot start it, or
does so holding a permanent admin key and deciding itself who may ask, an authorization system
reimplemented badly in a bot. **Korpis's chat client holds no admin key and never holds a user's
authority.** When a user runs a command, the platform delivers a signed interaction; the client
passes it to the gateway with the command's one action on one resource; the gateway has the identity
provider verify the interaction and say who the user is and which roles they hold, and hands the
client a ticket for that action, spent on the one call it makes, performed for the asserted user
([authentication](authentication.md)). If the user is not authorized, the control plane refuses; the
client never adjudicates. Had the client exchanged the user's identity for a token carrying their
grants, it would hold the authority of everyone who ever invoked it.

This is authority without accounts, possible because grants attenuate: a system with roles must
provision an account for a chat user before authorizing them, while one with grants binds an
external identity directly.

## How authority arrives

```
guild        -> organization      operator-declared: one guild bound to one organization
chat role    -> grant template    operator-declared: the role, as a set subject, is issued the
                                  template's grant, and its members exercise it
chat user    -> subject           a binding created on first authenticated use; confers nothing
```

A community already keeps its authority structure in chat (a `@moderator` role whose members should
restart the survival server), and Korpis reads it rather than asking for a duplicate. Binding the
role to a template issues one ordinary grant to the role as a set subject; members exercise it by
membership, nobody receives a copy, and it attenuates and audits as any grant. A member who leaves
the role stops at the next verification, and what they delegated is suspended with them ([external
identity](external-identity.md)). Nobody creates an account: a moderator who never opened the panel
restarts a server from their phone in the channel they were already in.

## Discord is a third party

- A guild administrator can assign roles, so if roles map to grants, a guild administrator can grant
  Korpis authority: a deliberate delegation the operator makes by binding the guild.
- A compromised chat account is a compromised subject; the platform's account security joins the
  operator's threat model.
- The platform sees command names and, by interaction type, arguments; a workload name is often a
  customer's.
- The platform can go down, be blocked in a jurisdiction, or change its API.

So, by default and overridable only by explicit operator declaration: **irreversible actions are
unavailable through chat roles**, enforced by the gateway at attenuation (grants to set subjects
exclude them), the client hiding them as a courtesy and offering a panel link; revoking a grant is
not on that list, since killing a leaked link from the channel it leaked in most needs to be fast.
And **`requires_mfa` is unsatisfiable through chat**, since Korpis cannot verify the platform's
second factor, so high-authority operations route to the panel by construction. Every operator runs
its own bot application with its own token; a shared public bot's token would reach every operator's
control plane.

## Plans as messages with buttons

Every change produces an inspectable plan first, and chat's native primitive (message, buttons,
result) fits:

```
/workload restart mc-survival
  Plan: restart mc-survival
  stop, then start on node fra-3; 14 players connected; ~40s unavailable
  authorized by your @moderator role
  [ Apply ]  [ Cancel ]
```

The button carries the plan's identifier, so applying applies that plan, not one recomputed since;
the card names the authority in words a member recognizes; pressing Apply is a new interaction
authorized as whoever pressed it. If the world moved, applying returns `CONFLICT` and the client
shows the new plan. Low-risk actions skip the button and post the plan with the result, the same
gradation as the panel's.

## Ephemeral by default

A channel is a broadcast surface: an address, logs, configuration or usage posted there is disclosed
to everyone present and everyone who scrolls back. **Every response containing state is ephemeral
unless its invoker makes it public**; the opposite default posts a customer's address to two hundred
people on the first day. Explanations, quota figures and node identities are filtered per viewer
before reaching a message.

## Console in chat

A console session is a single message edited in place, holding a rolling window, with Send command,
Pause and Open full buttons. Edits are coalesced to the platform's rate limits, so a workload
printing a thousand lines a second updates a message a few times a second, with a visible marker for
what was skipped. Send command opens a modal and needs `workload.console.write`. Open full is a
button, not a link: pressing it answers that person alone with a link minted under their authority,
since a bearer token in a channel is a grant to everyone who scrolls back. Interaction tokens expire
after fifteen minutes, so a live session is renewed explicitly, which is wanted: an abandoned live
console in a channel is a leak with a long tail. Scrollback is the stream's, not the message's.

## Notifications

**A channel is a subject** holding its own grant, issued by someone who could, and receiving exactly
the events that grant permits. Events scoped to whoever configured a subscription would keep leaking
after that person's access is revoked. Channel grants are read-only by construction, exercised only
for channels bound to the client's own installation, and a channel is never an actor, since people
press buttons and act as themselves. Revoking the channel's grant stops its notifications; auditing
what a channel sees is reading one grant. The client holds one event subscription through the
extension contract, filtered by every bound channel's grant. A thread per incident is an event
consumer writing through channel grants, needing nothing new.

## Commands are generated

Slash commands, arguments, types, autocomplete and permission hints are generated from the API
schema, since hand-written definitions drift within a release. Autocomplete resolves against what
the invoking user can read, so `/console mc` offers exactly the workloads that user may attach to,
and even a typo reveals no name.

## When the chat platform is down

Nothing in the data plane depends on it; the one thing that calls the platform is its identity
provider, re-verifying memberships. An outage removes a surface and changes nothing about whether
workloads run, converge or are reachable. Role-derived grants stay valid for the binding's grace
after the re-verification interval, then are suspended, not revoked, resuming when verification
succeeds: an outage degrades to a locked door after a stated time, never an open one.

## An extension, and other platforms

The client is a first-party extension registering `ext.` actions, subscribing to events and reading
streams under its own grants. If the contract cannot express a full client, the contract is broken,
and proving it can is worth more than building it in core; the machinery the bet rests on (bindings,
templates, attenuable grants, tokens) is core regardless, since OIDC, SAML, resellers and CI consume
it. A chat adapter provider interface is defined beside it, Discord its reference, so Matrix, Slack
and Telegram are ordinary third-party extensions. The interface must carry the widget vocabulary
(plan with buttons, an edited rolling console), and platforms that cannot express it degrade to
command and link, and say so. One guild maps to one organization; many organizations in one guild,
scoped by channel, is expressible and not designed.
