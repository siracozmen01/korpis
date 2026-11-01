# Panel

The web panel: a static bundle that is one client of the API among peers. It holds no secret, has
no server side, calls nothing the API does not publish, and can be replaced by a panel a host wrote
without the control plane noticing. If the panel can do something, it did it through a call any
other client can make. Its branding and landing layouts are on [branding](branding.md).

## Contents

- [Serving](#serving)
- [Routes and sessions](#routes-and-sessions)
- [Nothing on screen is a lie](#nothing-on-screen-is-a-lie)
- [Convergence is visible](#convergence-is-visible)
- [Plans scale their interruption, not their
  existence](#plans-scale-their-interruption-not-their-existence)
- [The panel is not the authorization boundary](#the-panel-is-not-the-authorization-boundary)
- [Sharing](#sharing)
- [Console](#console)
- [Files](#files)
- [Usage](#usage)
- [Reach](#reach)
- [Lost answers](#lost-answers)
- [Offline](#offline)
- [Translation](#translation)
- [The stack](#the-stack)
- [What it does not do](#what-it-does-not-do)

## Serving

The control plane serves the bundle itself from the directory `korpis-server serve --web-root`
names, on the API's own address, so an installation has a panel with nothing else to run and the
panel and the API share an origin. A path of the form `/package.Service/Method` is a procedure; a
path with no file and no extension is a panel route answered with `index.html`; any other path is a
file beneath the root, and one that climbs out, follows a link out, or names a hidden file is not
found. Files under `assets/`, whose names the build hashes, may be cached for a year; everything
else is revalidated. Every file is sent with `nosniff`, no referrer and a policy that no other page
may frame it. Without `--web-root` the address serves the API and `/about/source`, and says no
panel is served. Serving files beside the API runs every connection through the control plane's own
accept loop, the one that reads a PROXY header, rather than connectrpc's server, which serves only
procedures.

A panel served from another origin must be on the control plane's site (a sibling name under the
same registrable domain) and listed in `--web-origin`: the web session is a cookie the browser
sends only to its own site ([authentication](authentication.md)), and the API answers cross-origin
calls with credentials, preflights included, only for the installation's origin and the listed
ones. A panel on a CDN under an unrelated domain therefore holds no web session; a share link's
screen, which holds none, works from anywhere listed. At a [portal](portals.md) the control plane
serves the same bundle, the API and `/about/source` under a name an organization holds, and answers
cross-origin calls only for the portal's own origin.

## Routes and sessions

The routes are `/sign-in`, `/invitation`, `/g` (a share link's screen), `/device`, `/profile`, and
the places `/o/<id>`, `/p/<id>` and `/w/<id>`. The first three are open signed out; any other route
signed out leads to `/sign-in`, which returns afterwards only to a path of the panel's own. An
invitation arrives as `/invitation#korpisiv_...`, taken from the address bar at once and held in
memory.

`/device?code=...` is the page `korpis login` names: it shows the code, what the client says it is,
the address it asked from and when. Approving re-authenticates first when the last sign-in is older
than ten minutes; denying never does. One tab at a time exchanges the refresh cookie, under a Web
Lock, and shares the access token with the other tabs over a `BroadcastChannel`, so a rotated
cookie is never presented twice. A browser without Web Locks falls back to a lock per tab, where two
tabs exchanging at once end the session.

`/profile` shows the account's factors and sessions: passkeys, a password, an authenticator app (its
address as text and as a QR code drawn in the page), recovery codes renewed and shown once, SSH keys
added with the signature the command beside the form makes over the account (a public key pasted
alone proves nothing), and each session with where it began and when it was last used. A change
refused `session.not_recent` asks the person to confirm it is them and is then made. A workload's
files show, to whoever may read them, where an SFTP client connects and which host key to expect,
for each relay serving SFTP.

Signing in through an identity provider ([external identity](external-identity.md)): `/sign-in`
offers a button for each provider `ListSignInProviders` answers, and `/sign-in/<org>/<name>` offers
one. Either makes a PKCE verifier, keeps it and the return path in the tab's session storage, and
sends the browser where the control plane answers. `/sign-in/callback` completes with the state,
code and the verifier the tab kept, takes the pending sign-in once, and completes nothing in a tab
that kept none. A provider's own `error` is shown as it said it. A person signed in this way has no
factors, so `/profile` shows their subject, provider and sessions (`GetProfile` answers with their
binding and no account), and a recent sign-in is asked for by signing in through the provider again.

## Nothing on screen is a lie

Every value is in one of three states:

| State | Meaning | Rendered as |
|---|---|---|
| known | a current observation, heard live | the value |
| stale | the panel no longer hears it | the value, dimmed, with its age |
| unknown | no observation, or one the control plane says is not current | "unknown", with the last value and its age beside it |

Never `0`, never a dash, never a spinner that does not resolve. A workload's screen is its watch;
whether an observation is current is the control plane's decision (its node heard, see
[node agent](node-agent.md)). Ages grow on screen each second. A signed-in screen reopens a dropped
watch by itself, backing off from one second to thirty; a share link's screen asks, because each
opening spends a use. Lists show each workload's convergence as the list was read. No resource
figure is shown that cannot say its source, window and method (see [usage](#usage)).

## Convergence is visible

When a workload does not converge, the panel says at which stage, built from what the status holds:
applying names the step its node reported, or, before any, how long ago the target was declared and
that its node has not acknowledged; failing says it crashed (with the exit code) or why its node
cannot run it; not placed lists the checks that ruled out nodes ([placement](placement.md));
awaiting approval and scheduled say so, scheduled with when; unknown says how long the node has not
been heard. Disagreement between declared and observed is the normal state of a converging system
and is shown as such, with what Korpis is doing about it.

The control plane keeps no record of when a node acknowledged or converged beyond the effect log, so
the panel times each moment as it hears it, and a moment that passed before the screen opened is
shown as done, without a time it does not have. A node has acknowledged once it reports a step of
the active plan or has converged.

A `task` or `scheduled` workload's page lists its [runs](runs.md) in place of a console: what made
each (and its attempt past the first), its status, how it ended (the cause, the exit status, or how
many occurrences a missed run stands for), when it was due and started. The list is read again
whenever the watched status names another newest run or that run's status changes. A started run's
console opens from its row under `workload.console.read`; an active run is cancelled from its row
under `workload.stop` with the same `CancelRun` the CLI calls. A run's console is not followed into
the next run.

## Plans scale their interruption, not their existence

Every change produces a [plan](plans.md) first; a confirmation on every action trains people to
click through the one that mattered. So the plan is always computed, persisted and audited, and only
the interruption scales with risk. Every action is proposed (`propose` on start, stop, restart and
declare; a deletion always), and the panel classifies the plan from what it says, never from the
action's name, since a recipe can reclassify a step:

| The plan | The panel |
|---|---|
| every step reversible, nothing disruptive, no data at risk, one workload | confirmed at once, shown after |
| disruptive, or touching several workloads | previewed in place with Confirm and Cancel |
| data at risk or of unknown risk, or an irreversible step | its own screen, every step and its object listed, confirmed only after a box is ticked |

Confirming approves that plan by its identifier, so the plan shown is the plan that runs; cancelling
rejects it. A plan that needs someone else's authority is left waiting and the screen says so.

## The panel is not the authorization boundary

The control plane decides; the panel hides as a courtesy, never as a control. It must render
correctly for an arbitrary set of grants, a person holding `workload.console.read` on two workloads
through a share link included, so the least-authority view is built first and capability adds to
it. A workload the viewer cannot read is not shown as locked; it is not shown
([api](api.md)).

Navigation starts from the caller's own grants, every page of them, and for a person signed in
through an identity provider those of the sets that vouch for them too. Each working grant opens the
organization, project or workload it is anchored on (a selector grant, the object it selects
within); a grant on anything else is not a place. Lists show what the API returns, a page at a time
with the next offered, never cut at the first page. A place the caller cannot read is left out; a
read that failed for another reason is said.

An organization's page carries:

- **Suspension** ([tenancy](tenancy.md)): to everyone who reads it, that it is suspended, why, since
  when, whether its workloads are held stopped and what its tenant still reads. The form to suspend
  or resume is offered to a signed-in caller holding `org.suspend` or `org.resume` on a grant
  anchored above, never on a root.
- **Roles** ([grants](grants.md)): the grant templates offered there and above, what each allows,
  passes on and lasts. Anyone signed in who reads the place is offered to give one, to an account
  or by an invitation link shown once, because whether a giver can pass a role on depends on the
  whole chain only the control plane holds. `template.write` makes, edits against the version
  shown, and retires them.
- **Quota** ([quota](quota.md)): per entry what it counts and where, per dimension what it
  guarantees, sells, holds now and may sell below; what it gave below is read on asking, one call
  per organization beneath. The editor is offered under `org.quota.write` above, never on a root,
  writes labels rather than label expressions, and keeps expressions as read.
- **Identity bindings** ([external identity](external-identity.md)): each subject, provider, state,
  last confirmation, and interval and grace said in one sentence with their sum, the longest an
  unreachable provider extends it. `identity.binding.write` binds, edits, retires and asks the
  provider now; `org.write` sets the delegation ceiling.
- **Portals** ([portals](portals.md)): each with how its certificate stands; `portal.write` makes,
  changes and deletes them, deletion only after saying how many passkeys it removes and how many
  accounts would then sign in nowhere. A refusal `session.portal_confined` says to sign in at the
  provider's own address.
- **Audit** ([observability](observability.md)): what was done to anything in its subtree as far as
  the viewer's grants reach, a page at a time: when, the action, the outcome, who, the relaying
  client, refusals marked and coalesced repeats counted.

Every lever is offered where the caller's own grants suggest it and decided by the control plane,
whose refusal is said in its words. A share link is offered none of these, and is shown no audit,
since each call spends a use.

## Sharing

A share link's [capability token](capability-tokens.md) is in the URL fragment, never the path or
query, so it is never in a request line, an access log or a `Referer`. The panel reads it from
`location.hash`, holds it in memory and clears it from the address bar. `/g/#korpislk_...` opens a
screen containing exactly that workload and exactly those actions: every call carries the link and
no cookie, and the link's own grant says in words what it allows, until when and how many uses
remain. Reading the grant spends nothing; the workload's screen is its watch, one call, so opening
it spends one use however long it stays open. The console, files and usage open only when asked, by
a button saying it is a use, since each is a session of its own. A reload forgets the link and asks
for it to be opened again. A link no longer in force says which (used up, expired, revoked,
suspended) from its own grant and asks nothing of the object. A link for an organization or project
shows empty lists, because a listing spends nothing and so shows a use-limited grant nothing; the
share screen issues links for workloads.

The share screen is on a workload's screen, never a link's. It offers the actions in words, an
expiry of an hour, a day (the default) or a week, and a use cap (three by default), and says before
the link exists that opening and each action is one use and needs the control plane. The link is
shown once with a copy button. Beside it the grants anchored on the workload are listed, for a
caller who may read them, with holder, actions, state and expiry; revoking one says how many grants
attenuated from it went with it. A link never makes further links.

## Console

A durable, seekable [stream](streams.md), not a websocket into a memory buffer. The console mints a
token for `workload.console.read`, opens a session at the relay with `Watch` for the last 200
records and to follow, and renews the token through `Send` thirty seconds before it expires. A
dropped session reopens from the offset the node said to read next. Missing offsets and gap records
are drawn as gaps with what was lost and why. Only SGR colours and weights are kept, as classes from
a fixed set; every other escape sequence (titles, hyperlinks, clipboard writes, cursor moves) and
every control character but tab and line break is dropped. The view keeps the newest 2,000 lines and
says how many left it; Earlier reads the 200 before the oldest shown, from offloaded segments
through the control plane and from the node for the rest. The console's lines are written by a
windowed renderer directly, not through React. Reading mode holds the view still, turns its live
region off, and says how many lines arrived meanwhile, which is what makes a streaming console
usable with a screen reader. A typed line goes through a `workload.console.write` session of its
own; an input record is drawn as who typed what, set apart from the output.

## Files

The file manager is a client of the volume API and holds no privilege ([files](files.md)). It opens
one session for `workload.file.read` or `workload.file.write` at a relay, so a 40 GB upload never
crosses the control plane and the node accepts no connection. A browser cannot stream both ways, so
each request goes by `Send`, one at a time so the relay's numbering never repeats a frame, and each
answer heard on the `Watch` is matched to its request by number. Changes are offered only to a
caller who may write.

- A directory is listed 200 entries a page; a link is shown and never followed.
- A download asks a mebibyte at a time, four ahead, writing each to the file the browser lets the
  person pick, or, in a browser offering none, to a Blob saved at the end; the whole file is never
  in the page's own memory.
- An upload sends a mebibyte per request, four ahead, each chunk landing where the file ends, so a
  resume reads only what arrived in order. Onto a smaller file of the same name it asks whether to
  resume, replace or skip, since only the person knows whether it is the same file.
- Removing asks first, and says when a directory goes with everything in it.
- A request that could not be sent ends the session, since whether the relay took it is unknown, and
  another is opened; a stopped transfer says where it stopped.

## Usage

A billable number is never rendered without provenance ([metering](metering.md)). The workload's
Usage section has two parts side by side, never reconciled to each other:

- **Now** is its node's latest meter reading on a `metering.read` session through the relay: memory
  in use, and CPU, throttling, block and network bytes as rates, with when the node read it by its
  clock and the seconds each rate is over, and the words "Not billed, and not the figure below".
  The observation's `ResourceUsage` is not used, because an observation is sent when it changes and
  a figure changing every ten seconds would write a row every ten seconds for every workload.
- **Accounted** is the last day's records from `ListUsage`: CPU time, throttled time, GiB-hours of
  memory, the peak, bytes, egress, disk at the latest interval's end, and per endpoint where its
  accounting is on, under the window covered and how many intervals were measured, and "not counting
  N unheard over H hours" where any were not heard. How each figure was measured follows; each
  record is shown on request, a gap as unheard with its cause. A quantity nothing measured is left
  out, never shown as zero.

A caller without `metering.read` is told so and opens no session. A signed-in screen holds one more
session at the node while open; the day's records are read once, not followed. A file's size in the
file manager is the file's own, not a meter.

## Reach

Everything except large file operations works on a 400 px viewport, from one stylesheet loaded as a
file: a handful of tokens with a dark set chosen by the system's preference, one column of at most
60 rem with 16 px gutters, a header that wraps, forms one field wide, long values broken anywhere
rather than pushing the page sideways, and a focus ring on whatever the keyboard reaches. Every
control is a React Aria component, carrying keyboard behaviour and roles. The page's content
security policy names the control plane's origin when the build puts it elsewhere
(`VITE_KORPIS_API`).

## Lost answers

A phone moving between networks has its browser abandon every request in flight. The panel sends a
call the network failed with no answer again, twice, after a quarter of a second and after a second
(`web/src/retrying.ts`); that is safe because a mutation carries its idempotency key and a ceremony
spends a single-use proof ([api](api.md)). A share link's calls are sent once, since a lost answer
may have spent a use, and its screen offers to try again. A control plane truly unreachable is said
so a second and a quarter later.

A repeated ceremony's refusal is not the whole truth: if the first attempt arrived and only its
answer was lost, the repeat is refused because the first spent the proof, and the ceremony may have
happened. The panel marks a refusal that follows a lost answer (`korpis-earlier-answer-lost`, set by
`retrying.ts` and never sent by the control plane), and the ceremony's screen says what may have
happened: a redemption that the account may exist and to sign in with what was just registered, then
renew the recovery codes, never shown; a sign-in that it may have succeeded, with an offer to find
out. A refusal after an attempt that never arrived is then said as "may have", and going on finds
that it did not.

## Offline

The bundle may be cached; what it shows may not. A service worker built from `src/sw/worker.ts` as
`sw.js` carries the names of the bundle's content-hashed files and a cache named for them, so a new
bundle is a new worker that installs at the next load, takes over at once and deletes the old cache.
A bundle file is answered from the cache; a page is asked of the network first, so a reachable
control plane always serves the current panel and policy; every other request (the API, a relay,
the source notice) is never answered by the worker and never kept. Which is which is one function
with its own tests, and the build fails if the worker would import or export anything, since it is
loaded as a classic script. A development panel never registers it.

The panel keeps one thing of its own: when the control plane last answered this browser. A panel
loaded while the control plane cannot be reached shows every value unknown and how long it has been,
and asks again every 15 seconds. Libraries are chunks of their own, so a release changing only the
panel's code leaves them cached. A browser refusing the worker loads from the network every time; a
browser refuses it on an origin whose certificate it does not trust, so a person who clicks past the
warning for an installation's own CA, rather than trusting the CA, gets no offline panel and is not
told.

## Translation

Every string is a FormatJS message written where it is used, its English text beside an identifier
that hashes the text and its description, so a changed sentence is a new message a translation
cannot silently keep, and a word two screens use differently ("running" declared, "running"
observed) carries a description making it two. A sentence is one message with ICU plurals and
selects inside, never spliced fragments; lists, times, numbers, spans and byte counts use the
locale's formats. `pnpm extract` writes every message to `web/src/i18n/en.json`, which translators
work from; a translation is `web/src/i18n/catalogues/<locale>.json`. The locale is the first browser
preference a catalogue speaks, by exact tag or by language, else English, and the page's `lang` says
which. A message a catalogue lacks or cannot parse is shown in English, never as its identifier.

The lint refuses a literal string a person reads (text and `aria-label`, `alt`, `title`,
`placeholder`, `label`) and a message without its English text or with an identifier that is not its
hash; `scripts/check-web.sh` refuses an `en.json` differing from extraction, a translation naming a
message the source lacks, and one that does not compile. Every catalogue is in the bundle, so
nothing is fetched to show the first screen, at the price of the bundle growing per language.

[Recipes](recipes.md) ship their own ICU catalogues inside the artifact, versioned and signed with
it, so a recipe's translation is a new recipe version; the control plane serves them with the
recipe, and the panel resolves a string from the viewer's locale, then the recipe's default, never
showing a raw key. The control plane's own words, a refusal's message, are shown as it wrote them,
in English.

## The stack

React in TypeScript, built by Vite. Every candidate builds static files and consumes a stream; what
decides it is accessibility. React Aria (Apache-2.0) is the most complete set of accessible
primitives that exists, and exists for React alone; rebuilding it for another framework is a larger
project than the panel. React is also what most panel contributors already write. The cost is a
runtime of about 60 KB compressed against 10 to 20 for a compiled framework, which a slow phone's
first load notices, and re-rendering where a compiled framework updates a node, which is why the
console writes its rows directly. Clients are generated with Connect-ES from the same `.proto` as
the server, components tested with Vitest and Testing Library, and Playwright runs the panel against
a real control plane and node (`scripts/check.sh full`). Extension UI is never React in the panel:
it is an iframe on a separate origin ([extensions](extensions.md)), which also keeps it from the
viewer's session.

## What it does not do

- **Backups.** The panel shows no backup; listing, browsing, reading from and restoring one are the
  CLI's, though the API answers each ([backup](backup.md)). A game host's customer restores a world
  from a panel, so the screen is wanted before Korpis claims to be a credible game panel.
- **Translated refusals.** A refusal carries a stable `reason` and an English message; whether the
  panel translates by reason or the control plane learns the viewer's locale is decided before a
  second catalogue ships.
- **Bulk selection** is designed as one bulk operation with a plan per workload
  ([plans](plans.md)) and not yet in the panel.
