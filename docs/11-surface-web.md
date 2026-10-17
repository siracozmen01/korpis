# The Web Client

**Status:** design; framework and serving decided 2026-10-16 **Date:** 2026-08-07 **Depends
on:** [`10-api.md`](./10-api.md), [`08-identity.md`](./08-identity.md),
[`01-model.md`](./01-model.md) **Implements:** Principles P2, P4, P5, P10

---

## 1. The panel is a client

The web client is a static bundle. HTML, CSS, JavaScript, and nothing else. It holds no secrets,
has no server side, calls no endpoint that is not published in `10-api.md`, and can be deleted and
replaced by something a host wrote themselves without the control plane noticing.

This is P2 made physical rather than promised. In Pterodactyl the panel *is* the product and the
API is a surface bolted to its side; the asymmetry is visible in every operation the API cannot
perform. Here the asymmetry has nowhere to live: if the web client can do it, it did it through a
call any other client can make.

A consequence worth stating because hosts will want it: **the panel can be served from anywhere.**
A host can put it on their own CDN, at their own domain, with their own branding, pointed at their
control plane. Nothing about that is a special deployment mode; it is what a static bundle talking
to a public API already is.

**Where it is served by default, and what serving it elsewhere needs.** Decided 2026-10-16, before
the panel was built. The control plane serves the bundle itself, from the directory
`korpis-server serve --web-root` names, on the same address as the API: a request whose path is a
procedure of the API goes to the API, and any other path is a file of the bundle, so an
installation has a panel at its own address with nothing else to run, and the panel and the API
share an origin. Without `--web-root` the control plane serves the API alone and says so at start.
A panel served from another origin must be on the same site as the control plane (a sibling name
under the same registrable domain, such as `panel.example.com` beside `api.example.com`), and its
origin must be listed in `--web-origin`: the web session is a cookie the browser sends only to its
own site (§9.4 of `08-identity.md`), and the API answers cross-origin requests, with credentials,
only for the origins listed. What it costs: a panel on a CDN under an unrelated domain cannot hold
a web session with the control plane, and a host who wants one there serves it under a name of its
own site instead; a panel that holds no session, one opened from a share link, works from anywhere
listed.

> As built, 2026-10-17 (`korpis-server serve --web-root`). A path of the form
> `/package.Service/Method` is a procedure and goes to the API; `/about/source` is the source page
> below; any other path names a file beneath the root or nothing, one that climbs out of it,
> follows a link out of it, or reaches a hidden file being not found, and a path with no file and
> no extension is a route of the panel's own and is answered with its `index.html`. Files under
> `assets/`, whose names the build hashes, may be kept for a year; everything else is asked about
> again before it is used; every file is sent with `nosniff`, no referrer, and a policy that no
> other page may frame it. A cross-origin call is answered, with credentials, only for the
> installation's own origin and each `--web-origin`, preflights included, and refused for any
> other; the API's error, limit, and trailer headers are readable to the listed ones. Without
> `--web-root` the address answers the API and the source page, and every other path says no
> panel is served there. Serving files beside the API meant serving every connection through
> the control plane's own accept loop, the one a PROXY header already needed, rather than
> connectrpc's server, which serves nothing but procedures.
>
> As built, 2026-10-17 (`web/src`). The panel's own routes are `/sign-in`, `/invitation`, `/g`
> (a share link's screen, §6), `/device`, `/profile`, and the places `/o/<id>`, `/p/<id>`, and
> `/w/<id>` (§5); the first three are open signed out, and any other route, signed out, leads to
> `/sign-in` with the path it was going to, which signing in returns to only if it is a path of
> the panel's own. An invitation arrives as `/invitation#korpisiv_…`, taken from the address bar
> at once and held in memory (§6), or is pasted. `/device?code=…` is the page `korpis login`
> names (§9.4 of `08-identity.md`): it shows the code, what the client says it is, the address it
> asked from, and when, and approving it re-authenticates first when the last sign-in is older
> than ten minutes, while denying never does. One tab at a time exchanges the cookie, under a Web
> Lock, and the access token it gets is shared with the other tabs over a `BroadcastChannel`, so
> a rotated cookie is never presented twice. `/profile` shows the account's factors and its
> sessions: passkeys added and removed, a password set, changed, or removed, an authenticator app
> enrolled (its address as text and as a QR code drawn in the page) or removed, recovery codes
> renewed and shown once, and each session with where it began and when it was last used, ended
> one at a time or all but this one. A change refused `session.not_recent` asks the user to
> confirm it is them and is then made, so none is lost to the ten-minute rule (§9.4 of
> `08-identity.md`). A browser without Web Locks (no current one lacks
> them) falls back to a lock per tab, and there two tabs exchanging at once end the session.

---

## 2. Nothing on screen is a lie

P4 in interface terms. Every panel in this market renders unknown as zero, stale as current, and
pending as done, because those render more tidily. Korpis renders three states everywhere a value
appears:

| State | Meaning | Rendered as |
|---|---|---|
| known | observed within the freshness window | the value |
| stale | last observation is older than the window | the value, visibly dimmed, with its age |
| unknown | never observed, or the node is unreachable | **"unknown"**, not `0`, not `, `, not a spinner |

A spinner that never resolves is the same lie as a wrong number, told more slowly. If the agent has
not reported, the interface says so and says for how long.

Memory, disk, network, uptime, and player counts all carry an observation timestamp (§3.3 of
`01-model.md`). The interface shows it. A CPU graph that flatlines because a node went away looks
identical to one that flatlines because a process idled, unless the interface distinguishes them,
and here it does.

> As built, 2026-10-17 (`web/src/workload`). A workload's screen is its watch. Known is a current
> observation, which the control plane decides (its node heard, §4.3 of `02-architecture.md`),
> heard live; stale is the panel no longer hearing, the value dimmed with how long ago it was
> heard; unknown is no observation, or one the control plane says is not current, shown as
> "unknown" with the last one and its age beside it. Ages grow on screen each second. A signed-in screen opens a dropped watch again by itself,
> waiting from one second up to thirty; a share link's screen asks, because each opening spends
> a use. The lists of workloads show each one's convergence as the list was read.
>
> Corrected 2026-10-17. This note said a resource not reported is shown as unknown, never 0, and
> the screen showed CPU, memory, and disk rows. Nothing in Phase 1 reports them, so the rows only
> ever said "unknown", and a figure that did arrive would have carried none of what §8 requires of
> it. The screen shows the observed state and health, and no resource figure until §8 can be met.

---

## 3. Convergence is visible

This is the single thing no existing panel shows, and it falls out of the model for free.

`Intent` carries a version; the agent reports `intent_seen` (§4.3 of `02-architecture.md`). The
difference between them is the convergence lag, and it is a number the interface can print:

```
Restart requested          14:32:07   intent v41
Node acknowledged          14:32:07   +0.3s
Converged                  14:32:19   +12s
```

When it does not converge, the interface says which stage it is stuck at, rather than showing a
button that did nothing. "Declared 40 seconds ago, node `fra-3` has not acknowledged" is a
diagnosis. A greyed-out button is not.

A workload whose observed state disagrees with its declared state is not an error condition to be
hidden; it is the normal, temporary state of a converging system, and it is displayed as such, with
the reason if one is known, and with what Korpis is currently doing about it.

> As built, 2026-10-17. The stage is the convergence the control plane computed, and the
> sentence is built from what the status holds: applying names the step its node reported, or,
> before any, how long ago the target was declared and that its node has not acknowledged;
> failing says it crashed (with the exit code) or why its node cannot run it; not placed lists
> the checks that ruled out nodes; awaiting approval and scheduled say so, scheduled with when;
> unknown says how long the node has not been heard. The control plane keeps no record of when
> a node acknowledged or converged beyond the effect log, so the panel times each moment as it
> hears it, at the observation that first showed it, and a moment that passed before the
> screen opened is shown as done, without a time it does not have. A node counts as having
> acknowledged once it reports a step of the active plan or has converged.

---

## 4. The Plan, without making everything a two-step

P5 says every change produces a Plan before it produces an Effect. Applied naively that turns
"restart this server" into a confirmation dialog, and a confirmation dialog on every action trains
people to click through confirmation dialogs, which destroys the one on the action that mattered.

**The Plan is always computed, always persisted, always in the audit trail. What scales with risk
is the interruption, not the Plan.**

| Plan character | Interface |
|---|---|
| reversible, single workload, no data loss | applied directly; the Plan is rendered **after**, in the activity feed, and remains inspectable |
| service interruption, or affects several workloads | inline preview expands in place; one confirmation |
| destroys data, changes authority, or crosses a tenancy boundary | full Plan screen; explicit confirmation; the affected-object list is enumerated, not summarized |

Deleting a volume shows what is on it and what would still reference it. A drain shows every
workload that would move and where each would land, with the `Explanation` from §2 of
`05-scheduling.md`, filtered to what the viewer is allowed to know (§8 of `08-identity.md`).

Because the Plan is a real object, "show me what this would do" and "schedule this for 03:00" and
"send this to someone for approval" are the same feature seen from three angles, rather than three
features.

> As built, 2026-10-17 (`web/src/workload/Actions.tsx`). Every action is proposed: a power change
> with `propose` (`StartRequest`, `StopRequest`, and `RestartRequest` gained it as
> `DeclareIntentRequest` has it), a deletion always. The plan the control plane computed and kept
> is then classified from what it says, never from the action's name, since a recipe can
> reclassify a step: every step reversible, nothing disruptive, no data at risk, and one workload
> is confirmed at once and shown after; anything disruptive or touching several workloads is
> previewed in place with Confirm and Cancel; data at risk or of unknown risk, or an
> irreversible step, takes the screen, lists every step and the object it acts on, and confirms
> only after a box saying so is ticked. Confirming approves that plan by its identifier, so the
> plan shown is the plan that runs; cancelling rejects it. A plan that needs someone else's
> authority is left waiting and the screen says so. A share link's screen offers only the
> actions the link holds, and a proposal it confirms costs it one use, not two (§6.1 of
> `08-identity.md`); a signed-in screen offers every action and the control plane decides (§5).

---

## 5. The interface is not the authorization boundary

The server decides. The interface hides things as a courtesy, never as a control. Every action the
UI renders is authorized again on arrival, and an action the UI declined to render is denied just
as firmly if it is called directly.

This has a design consequence that is easy to skip and expensive to retrofit: **the interface must
render correctly for a subject holding an arbitrary set of grants.** Not "admin" and "user", a
person with `workload.console.read` on two workloads and nothing else, arriving through a share
link with no account at all, must get a coherent screen and not a broken admin layout with most of
it greyed out.

The practical rule: build the least-authority view first and let capability add to it. Every
existing panel in this market did the opposite, which is why their non-admin views look like an
admin view with holes cut in it.

Existence follows read authority (§8 of `10-api.md`), so a workload the viewer cannot read is not
rendered as locked. It is not rendered.

> As built, 2026-10-17 (`web/src/views/Places.tsx`). The navigation starts from the caller's own
> grants, every page of them, each working one opening the organization, project, or workload it
> is anchored on (a selector grant, the object it selects within); a grant on anything else is
> not a place and is not shown as one. An organization lists its organizations and projects and a
> project its workloads, each list only what the API returns, a page at a time with the next
> offered while one exists, never cut at the first page as if it were all. A place the caller
> cannot read is left out; a read that failed for another reason is said, not hidden.

---

## 6. Sharing

§6.1 of `08-identity.md` puts the capability token in the URL **fragment**, never the path or
query, so it is never sent to the server in a request line, never lands in an access log, and never
leaks through `Referer`. The client reads it from `location.hash`, holds it in memory, and clears
it from the address bar.

The share flow is a first-class screen, not a hidden feature, because it is the operation P6 exists
to make possible:

```
Share "mc-survival"
  they can          [x] view console   [ ] send commands   [ ] restart   [ ] files
  expires           in 24 hours
  uses              3   (a use is opening it; needs the control plane to be reachable)
  requires sign-in  no
  →  https://panel.example/g/#korpislk_7f3a…
```

The resulting link opens a screen containing exactly that workload and exactly those actions. There
is no account, no invitation email, and no membership record, and revoking it is revoking one
grant. The dialog shows the use cap and its cost before the link exists, because §6.1 of
`08-identity.md` makes caps a default and §6.2 makes them depend on the control plane; the first
version of this screen showed neither (finding S29). A link can never be used to make further links
(§3.2 of `08-identity.md`), and the screen does not offer it.

> As built, 2026-10-17 (`web/src/views/LinkShell.tsx`). `/g/#korpislk_…` opens the link's screen:
> every call carries the link and no cookie, the link's own grant says in words what it allows,
> until when, and how many uses in all, and the object it is for is shown under `/g`. Reading
> the grant spends nothing; a workload's screen is its watch, one call, so opening it spends one
> use however long it stays open (Finding 76 of `23-walkthroughs.md`). A reload forgets the link,
> which lives only in memory, and the screen asks for it to be opened again rather than failing.
> A link for an organization or a project shows its lists empty, because a listing spends nothing
> and so shows a use-limited grant nothing (§6.1 of `08-identity.md`); a link is for one object,
> and the screen that issues links (P1.12e6) issues them for workloads.
>
> The share screen is on a workload's screen, never on a share link's. It offers the actions in
> words, an expiry of an hour, a day (the default), or a week, and a use cap (three by default),
> and says before the link exists that opening its screen and each action is one use and that
> using it needs the control plane. The link, `/g/#korpislk_…` on the panel's own address, is
> shown once with a button to copy it. Beside it, the grants anchored on the workload are
> listed for a caller who may read them, each with its holder, actions, state, and expiry, and
> revoking one says how many grants attenuated from it went with it.

---

## 7. Branding

White-labelling is a paid feature in the commercial products here. P10 forbids gating it, so it is
core, complete, and free: name, logo, colours, domain, login page, email templates, and the footer.

**Branding is configuration, not patched files.** Rule K-6 and P8: the reason Pterodactyl's
ecosystem standardized on `sed`-patching core files is that there was no supported way to change
anything. Here themes are data (a token set plus optional CSS) resolved per organization, so a
reseller's customers see the reseller's brand without anyone modifying a deployed file. Extensions
contribute UI through declared slots (`16-extensions.md`), never by editing the bundle.

**White-labelling stops at one fixed path.** The web client's JavaScript is conveyed to every
browser that loads it, and AGPL-3.0 attaches its source obligation to that (§2 of
`19-governance.md`). So every build serves `/about/source`, naming the exact source revision it was
built from and where that source is published, and themes cannot remove or restyle it. It is not in
the branded chrome: a host's customers see the host's brand everywhere they look, and the notice is
where the licence needs it to be rather than where a brand would notice it.

> As built, 2026-10-17. The control plane answers `/about/source` on its address whether or not
> it serves a panel, unstyled and under a policy that runs no script and loads no style, naming
> its own version, revision, and source, and the panel's revision and source when the bundle in
> `--web-root` says them in the `about/source.json` its build writes. The bundle also carries
> `about/source/index.html`, the same said for itself, so a panel served from a CDN meets the
> licence without a control plane in front of it.

**What a theme is, designed 2026-10-16.** A `Theme` belongs to an organization and is inherited by
everything beneath it unless a descendant sets its own, as a layout is (question 2): a product
name, a logo, a fixed set of colour tokens for the light and the dark scheme, the text of the
sign-in page, and optionally a stylesheet of at most 64 KiB. The panel applies the theme of the
viewer's provider, the top organization the viewer can read, whatever they are looking at; before
anyone has signed in, it applies the root organization's. A reseller's sign-in page at the
reseller's own address needs the control plane to answer there, which Phase 2 designs with the
names organizations hold for their customers (Finding 66 of `23-walkthroughs.md`); the theme of the
organization holding the verified domain the panel was reached at, which the browser states in its
`Origin`, is applied then. Setting one is `org.write` on the organization. A theme is read through
the API like everything else, with the logo inside it, so the panel fetches nothing a theme names.

**A stylesheet is code that runs in the panel, and it is fenced.** The panel's content security
policy, which the bundle carries in its own page so it holds wherever the bundle is served, admits
images, fonts, and connections from the panel's own origin, the control plane, and its relays,
and nothing else, so a stylesheet cannot send what it can see to a server of its author's: a
selector that matches a value on screen and loads a background image from elsewhere is refused by
the browser. What it can still do is restyle what the viewer sees, which a reseller styling its
customers' panel is meant to; and it cannot reach `/about/source`, which is served unstyled. What
it costs: a host who wants a font or an image from a CDN of its own serves it through the theme's
own data instead. And because a rule can hide, move, or replace any text the panel shows, the
stylesheet is never worn where what the page says is the viewer's own business rather than the
brand's: the profile, with its passwords, passkeys, and sessions, and a share link's page, whose
holder is told what the link allows, wear a theme's colours, name, and logo alone, and the device
approval wears no theme at all. The colours are held to a contrast that keeps text readable. Email
templates are part of a theme when Korpis sends email, which it does not
in Phase 1: there is no emailed reset (§9.3 of `08-identity.md`), and invitations are handed over
by whoever issues them.

> As built, 2026-10-17. `SetTheme` on the
> organization service, under `org.write`, sets or removes an organization's own theme against the
> version `GetTheme` returned, and every change is an effect, `org.theme.set`. `GetTheme` answers
> for an organization, a project, or a workload, under that object's read action, with the theme
> of its organization or of the nearest ancestor that set one and the organization it came from;
> reading it spends no use of a share link's grant, since the page it styles spent the one it
> costs. `ServerInfoService.GetSignInTheme` answers anyone with the installation's root
> organization's theme, the first organization made with no parent, charged to the caller's
> address like every other call made without a credential (§10 of `10-api.md`). Every field is
> checked where it is set and again where it is read back: a product name of at most 64
> characters on one line, a logo that is the PNG, WebP, or SVG its media type says and at most
> 256 KiB, colours only from the panel's nine tokens and only as `#rrggbb`, sign-in text of at
> most 2,000 characters, and a stylesheet of at most 64 KiB with no `@import` and no backslash
> escape, since an escape is how CSS spells `@import` past a check. `korpis theme
> show|set|clear` is the command (§2 of `13-surface-cli.md`).
>
> The panel wears it (`web/src/theme`). A signed-in viewer wears, on every page, the theme of
> the one organization they can read whose parent they cannot, the top of what their grants
> reach; with no such organization, or more than one, the installation root's. A share link's
> page wears the theme `GetTheme` answers for the place it opens, without its stylesheet, and so
> does the profile; the sign-in and invitation pages, before anyone has signed in, the
> installation root's. A colour is refused where it is set if text on the background or on a
> surface, muted text or a danger notice on the background, or text on the accent would fall
> below WCAG's 4.5:1 for text, each token a theme leaves unset taken as the panel's own (a test
> holds that table to the panel's stylesheet). Its colours
> become the panel's custom properties, the light scheme on the root and the dark in its media
> query, and only a `#rrggbb` value of one of the nine tokens is written, whatever the answer
> held; its stylesheet follows. Both are stylesheets the page constructs and adopts rather than
> loads or inlines, so the policy keeps `style-src 'self'` and admits no inline style, and a
> constructed sheet ignores `@import` besides. Its name is the page's title and stands with its
> logo in the chrome; the logo is shown as an image from a `data:` address, which the policy
> admits for images alone and where an SVG runs nothing; the sign-in text is shown as text. A
> test reads the policy out of the page and fails if it ever admits a script or a style from
> anywhere else, or anything `unsafe`. The organization's page offers its theme for editing to
> whoever opens it, the control plane deciding who may save: each field, a preview worn on the
> page before saving, a save against the version it was read at, and removing the
> organization's own theme so the nearest above applies again.
>
> Corrected 2026-10-17 (Finding 83 of `23-walkthroughs.md`). As first built, a place wore the
> theme of the organization it belonged to, and the start page the theme of whichever
> organization the list answered first, with the stylesheet worn on every page. That let a
> tenant dress its provider's screens: a host operator opening a customer's workload saw the
> customer's stylesheet, which could hide a warning, restyle a destructive button, or move text
> on the operator's own panel, and a share link's disclosure and the profile could be restyled
> the same way. A theme is a provider's brand for the people it serves, so it follows the viewer,
> not the object; the cost is that a host's operator never sees a customer's brand while working
> inside the customer's organization, and a reseller previews a customer's theme only with the
> editor's preview.

As decided 2026-10-16: the page is generated when the bundle is built, from the revision it is
built from and the repository the build names, and is a file of the bundle like any other, so it
is served wherever the bundle is; a host that modifies the panel and builds it names its own
repository, which is the obligation the AGPL places on it.

---

## 8. The hard screens

Three screens carry most of the risk, and each is where competitors' architecture shows through.

**Console.** A durable, seekable stream (`14-streams.md`), not a websocket into a memory buffer. It
survives reload, supports scrollback beyond the session, marks gaps explicitly (§7 of `10-api.md`),
and renders ANSI without letting a workload's output execute anything. A workload that prints ten
thousand lines a second must not take the browser tab down with it, the client renders a windowed
view and drops with a visible marker, never silently.

> As built, 2026-10-17 (`web/src/console`). The console mints a token for
> `workload.console.read`, opens the session at the relay with `Watch` (§3.1 of
> `02-architecture.md`) asking for the last 200 records and to follow, and renews the token
> through `Send` thirty seconds before it expires. A dropped session reopens from the offset the
> node said to read next, by itself for a signed-in user and on request through a share link,
> since each opening is a use. Missing offsets and gap records are drawn as gaps with what was
> lost and why. Only SGR colours and weights are kept, as classes from a fixed set; every other
> escape sequence (a title, a hyperlink, a clipboard write, a cursor move) and every control
> character but a tab and a line break is dropped. The view keeps the newest 2,000 lines and says
> how many left it; Earlier reads the 200 before the oldest shown, from the offloaded segments
> through the control plane and from the node for what is not offloaded yet. Reading mode holds
> the view still, turns its live region off, and says how many lines arrived meanwhile. A line
> typed goes through a `workload.console.write` session of its own, offered where the caller may.
> An input record is drawn as who typed what, set apart from the workload's output (§6 of
> `14-streams.md`), not as the JSON body the stream writer recorded; until the browser test (§9 of
> `21-stack.md`) found it on 2026-10-17, the body was shown as it was.

**Files.** The web file manager is a client of the volume API and holds no privilege of its own.
Large files stream rather than buffer; a workload's directory is not something a browser tab should
try to hold. Upload goes through a relay with a scoped, short-lived token (§3.1 of
`02-architecture.md`), streamed and resumable by chunk, so a 40 GB world upload never traverses the
control plane and never needs the node to accept a connection. The first version said
"direct-to-node", which a node with no inbound port cannot accept (finding S19). All the interesting
failures here are on the server side of the boundary, and §3 of `06-storage.md` and `17-security.md`
are where they are addressed, the interface's own obligation is simply never to become a privileged
path.

What a file session can do, how an upload resumes, and what is recorded were designed on
2026-10-16 in §3.2 of `06-storage.md`; a browser carries the session with the relay's `Watch` and
`Send` (§3.1 of `02-architecture.md`), since it cannot stream both ways.

> As built, 2026-10-17 (`web/src/files`). The workload's screen shows the file manager to a caller
> who may read its files, and offers changes only to one who may write them. It opens one session
> for `workload.file.read` or `workload.file.write`, sends each request with `Send`, one call at
> a time so the relay's numbering never repeats a frame, and matches each answer heard on the
> `Watch` to its request by number; a lost session is opened again by itself for a signed-in
> user and on request through a share link, at the directory last shown. A directory is listed
> two hundred entries a page; a link is shown and never followed. A download asks a mebibyte at a
> time, four ahead, and writes each to the file the browser lets the person pick as it arrives,
> or, in a browser that offers none, keeps it as a Blob saved at the end, which the browser may
> hold on disk; the whole file is never in the page's own memory either way. An upload sends a
> mebibyte per request, four ahead, replacing what is there only from its first byte; onto a
> smaller file of the same name it asks whether to resume from what is there, to replace it, or
> to skip it, since only the person knows whether it is the same file. Removing asks first, and
> says when a directory goes with everything in it.

**Metering.** Numbers people will be billed on. Every figure names its source, its window, and its
collection method, and disagreements between the instantaneous and the accounted value are shown
rather than reconciled behind the scenes. `15-observability.md` defines what is measured; the
interface's rule is that a billable number is never rendered without provenance.

> As built, 2026-10-17: by showing none. An audit of every figure the panel shows found two kinds.
> A file's size in the file manager is the file's own, read from the volume when it is listed,
> and is no meter. A workload's CPU, memory, and disk come from the observation's
> `ResourceUsage`, which nothing in Phase 1 fills and which carries no window and no collection
> method to show; the measurements are the node's metric stream and the meters of §2 and §3 of
> `15-observability.md`, built in Phase 2. So the workload's screen shows no resource figure,
> rather than a row that only ever says "unknown" or a number it cannot account for. What it
> costs: an operator reads usage from the node until Phase 2, when each figure arrives with its
> source, its window, and its method, and an accounted figure beside an instantaneous one shows
> both rather than one reconciled to the other.

---

## 9. Mobile and reach

Hosting customers restart servers from phones, and community moderators do it during an incident.
Everything except large file operations must work on a 400 px viewport, not through a separate app,
which would be a second surface with a second set of gaps, but because the layout was built for it.

Keyboard navigation is complete. The console is the genuinely hard accessibility problem, a live
region that streams thousands of lines is hostile to a screen reader, and is solved with an
explicit paused-and-buffered reading mode rather than by leaving it unsolved.

> As built, 2026-10-17 (`web/src/styles.css`). One stylesheet, loaded as a file so the page's
> policy keeps `style-src 'self'`: a handful of tokens with a dark set chosen by the system's
> preference, one column of at most 60 rem with 16 px gutters, a header that wraps, forms one
> field wide, long values (links, addresses, identifiers) broken anywhere rather than pushing
> the page sideways, and a focus ring on whatever the keyboard reaches. Every control is a React
> Aria component, which carries the keyboard behaviour and roles. Branding (§7) will replace the
> tokens, not the rules. The page's policy also names the control plane's origin when the build
> puts it elsewhere (`VITE_KORPIS_API`): the policy's own comment promised that and the policy
> did not say it, so a panel built that way could not have reached its control plane.
>
> A phone that moves between networks has its browser abandon every request in flight, and so
> does a machine whose interfaces change: until 2026-10-17 every screen that was loading then said
> "Failed to fetch" until the page was loaded again (Finding 88 of `23-walkthroughs.md`). The
> panel now sends a call the network failed with no answer again, twice, after a quarter of a
> second and after a second (`web/src/retrying.ts`), which is safe because a mutation carries its
> idempotency key and a ceremony spends a single-use proof (§6 of `10-api.md`). A share link's
> calls are the exception and are sent once, since a call whose answer was lost may have spent a
> use; its screen offers to try again instead. What it costs: a control plane that is truly
> unreachable is said so a second and a quarter later.

---

## 10. Open questions

1. **Framework. Resolved 2026-10-16: React, in TypeScript, built by Vite.** Deferred deliberately
   until the panel was next to be built; the constraints were already fixed, static output, no
   server runtime, streams over polling, and small enough that a host serving it from a cheap CDN
   is ordinary.

Every candidate meets those four (React, Svelte, Solid, and Lit all build to static files and
consume a server stream), so they do not decide it. What decides it is §9 and §5. Keyboard
navigation that is complete, and dialogs, menus, comboboxes, tables, and date fields that a screen
reader can operate, is the requirement every panel in this market fails, and the most complete set
of accessible primitives that exists, React Aria (Adobe, Apache-2.0), exists for React alone;
building that set again for another framework is a larger project than the panel. A panel that
renders correctly for an arbitrary grant set is a composition of small components that each
render or do not, which every candidate does equally well. And the panel is where most community
contributions to a panel land, and React is the framework most of those contributors already
write, Pterodactyl's own panel included (`research/evidence.md` §5).

What it costs. React's runtime is larger than what Svelte or Solid compile to, about 60 KB
compressed against 10 to 20, which a CDN serves without noticing and a first load on a slow phone
does notice. Its rendering model re-renders components where a compiled framework updates a node,
which matters only where values change many times a second: so the console does not render its
lines through React (§8); a windowed renderer writes the visible rows directly and React owns the
frame around it. And extension UI is not React and must not be: an extension's custom UI is an
iframe on a separate origin (§5 of `16-extensions.md`), written in whatever its author likes,
which is also what keeps it from reading the viewer's session.

The rest of the toolchain follows from it and is fixed in §9 of `21-stack.md`: Connect-ES with
clients generated from the same `.proto` as the server (§3 of `10-api.md`), FormatJS for the ICU
MessageFormat catalogues of question 4, Vitest and Testing Library for components, and
Playwright for the browser tests that run the panel against a real control plane.
2. **Custom dashboards. Resolved here: a saved layout object over declared slots.** Hosts want a
   landing screen composed for their customers. Whether that is an extension slot, a saved layout
   object, or both determines whether it can be per-organization. It was routed to
   `16-extensions.md`, which never carried it (finding S18).

A dashboard is a `Layout`: a named arrangement of panel components and extension slots (§5 of
`16-extensions.md`), owned by an organization, inherited by its subtree unless a child sets its
own. It is data, so it is per-organization by construction, and it can only place what already
exists, so it cannot become a way around the slot inventory. Every component on it renders under
the viewer's own authority, so a host's dashboard shown to a customer shows that customer's view.

Phased 2026-10-16: Phase 2 (§4 of `20-roadmap.md`). A layout is a host composing a landing screen
for its customers, a relationship Phase 2 creates, and what it can place is the slot inventory,
which question 4 of §9 of `16-extensions.md` says is driven by the core extensions' needs, most of
which arrive after Phase 1. Phase 1's panel has one landing screen, its own.
3. **Offline and PWA. Resolved 2026-10-16: the panel may be cached, what it shows may not.**
   Attractive for the incident case, the panel loading while the control plane is degraded.
   Requires deciding what a cached view is allowed to claim, which collides with §2.

The bundle is cached by a service worker, so a browser that has loaded the panel once loads it
again when the control plane cannot be reached; no value the API returned is ever cached. A panel
loaded that way shows every value as unknown, with how long the control plane has been unreachable,
which is §2 applied to the whole screen, and the incident it was wanted for at least gets a true
account of itself instead of a browser's error page. A new bundle replaces the cached one at the
next load once it is served, so an upgrade never leaves a browser on an old panel past one reload.
What it costs: a service worker, which a browser keeps until the panel removes it, and a panel that
loads while it can do nothing, which is what the incident case asked for. Built in Phase 1 with
the panel.

> As built, 2026-10-17 (`web/src/sw`, `web/src/offline`). The worker is built from
> `src/sw/worker.ts` as `sw.js` beside the page, with the names of the bundle's files, each named
> by its content's hash, written into it by the build and its cache named for them, so a new bundle
> is a new worker: the browser installs it at the next load, it takes over at once, and it deletes
> the old bundle's cache. A file of the bundle is answered from that cache; a page is asked of the
> network first, so a reachable control plane always serves the current panel and its current
> policy, and the cached page answers only when the network cannot; every other request, every
> API call, a relay, the source notice, is never answered by the worker and never kept. Which is
> which is one function with its own tests, and the build fails if the worker would import or
> export anything, since it is loaded as a classic script. A built panel registers it, a panel in
> development never does. The panel keeps one thing of its own: when the control plane last
> answered this browser, a time and nothing it said, so the page it shows while the control plane
> cannot be reached says every value is unknown and how long it has been, and asks again every
> 15 seconds. The libraries the panel is built on are chunks of their own, so a release that
> changes only the panel's code leaves a browser's copy of them valid. What it costs: a browser
> that refuses the worker loads the panel from the network every time, and the worker's source
> map is offset where the build wrote the file names in.
4. **Internationalization. Resolved here.** Community translation is one of the reasons Pterodactyl
   spread. The decision is whether recipe-supplied strings (§5 of `09-recipes.md`) participate in
   the same translation pipeline as core strings, which is not obvious. It was routed to
   `09-recipes.md`, which never carried it (finding S18).

They participate in the same message format and the same tooling, and not the same repository.
Core strings and recipe strings are both ICU MessageFormat catalogues keyed by stable identifiers.
A recipe ships its catalogues inside its artifact, so they are versioned and signed with it, and a
translation of a recipe is a new recipe version rather than a file someone else's server fetches at
runtime. The panel resolves a string from the recipe's catalogue for the viewer's locale, then the
recipe's default locale, and never displays a raw key. Community translators work on core and on
first-party recipes with the same tools; third-party recipes are their authors' to translate.

Built in Phase 1 with the panel, decided 2026-10-16: every string the panel shows comes from a
catalogue, English first, and the recipe catalogues of §5 of `09-recipes.md` are read and served
by the control plane with the recipe they came in, so the panel needs no second source.

> As built, 2026-10-17 (`web/src/i18n`). Every string the panel shows is a FormatJS message
> written where it is used, its English text beside a stable identifier that is a hash of that
> text and its description, so a changed sentence is a new message a translation cannot silently
> keep the old meaning of, and a word that two screens use differently ("running" declared,
> "running" observed) carries a description that makes it two. A sentence is one message, with
> ICU plurals and selects inside it, never English fragments spliced together, so a language can
> order and inflect it; a list is joined by the locale's list format, and a time, a number, a span,
> or a byte count is the locale's own. `pnpm extract` writes every message with its description
> to `web/src/i18n/en.json`, the file translators work from; a translation is
> `web/src/i18n/catalogues/<locale>.json`, identifier to text. The locale is the first of the
> browser's preferences that a catalogue speaks, by its exact tag or by its language alone, and
> English otherwise, and the page's `lang` says which. A message a catalogue lacks, or holds in
> ICU that does not parse, is shown in English, never as its identifier. The lint refuses a
> literal string in the markup a person reads (text and the `aria-label`, `alt`, `title`,
> `placeholder`, and `label` props) and a message without its English text or with an identifier
> that is not its hash; `scripts/check-web.sh` refuses an `en.json` that differs from what
> extracting the source writes, so no message is missing from it and none is unused, a
> translation naming a message the source lacks, and a translation that does not compile. A
> space between a value and a remark on it is drawn by the stylesheet rather than written in a
> message, and a path's separators are shown as they are. English is the only catalogue so far.
> What it costs: every catalogue is in the bundle, so the locale is known before anything renders
> and nothing is fetched to show the first screen, at the price of the bundle growing with each
> language until catalogues are worth splitting by locale; and the control plane's own words, a
> refusal's message, are shown as it wrote them (question 6).
5. **Bulk selection. Resolved in §3 of `05-scheduling.md`**: a selection becomes a `bulk_intent`
   Operation, one approval, a plan per workload, and per-item results. The interface will want
   "select 200 workloads and restart them", which needs the bulk mutation semantics that were open
   in §12 of `10-api.md` and the `Operation` object of `05-scheduling.md`.
6. **The control plane's own words.** A refusal carries a stable `reason` (§8 of `10-api.md`)
   and a message the control plane writes in English, and the panel shows the message where it has
   no sentence of its own for the refusal. Translating refusals by their reason in the panel's
   catalogue would put every reason the control plane can give into the panel's source, where a
   reason added by a newer control plane would show untranslated anyway; carrying the viewer's
   locale to the control plane would put catalogues in the server. Which, and whether a recipe's
   refusals join it, is decided before a second catalogue ships. → here
