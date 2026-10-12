# The Web Client

**Status:** design **Date:** 2026-08-07 **Depends on:** [`10-api.md`](./10-api.md),
[`08-identity.md`](./08-identity.md), [`01-model.md`](./01-model.md) **Implements:** Principles P2,
P4, P5, P10

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
  requires sign-in  no
  →  https://panel.example/w/mc-survival#g_7f3a…
```

The resulting link opens a screen containing exactly that workload and exactly those actions. There
is no account, no invitation email, and no membership record, and revoking it is revoking one
grant.

---

## 7. Branding

White-labelling is a paid feature in the commercial products here. P10 forbids gating it, so it is
core, complete, and free: name, logo, colours, domain, login page, email templates, and the footer.

**Branding is configuration, not patched files.** Rule K-6 and P8: the reason Pterodactyl's
ecosystem standardized on `sed`-patching core files is that there was no supported way to change
anything. Here themes are data (a token set plus optional CSS) resolved per organization, so a
reseller's customers see the reseller's brand without anyone modifying a deployed file. Extensions
contribute UI through declared slots (`16-extensions.md`), never by editing the bundle.

---

## 8. The hard screens

Three screens carry most of the risk, and each is where competitors' architecture shows through.

**Console.** A durable, seekable stream (`14-streams.md`), not a websocket into a memory buffer. It
survives reload, supports scrollback beyond the session, marks gaps explicitly (§7 of `10-api.md`),
and renders ANSI without letting a workload's output execute anything. A workload that prints ten
thousand lines a second must not take the browser tab down with it, the client renders a windowed
view and drops with a visible marker, never silently.

**Files.** The web file manager is a client of the volume API and holds no privilege of its own.
Large files stream rather than buffer; a workload's directory is not something a browser tab should
try to hold. Upload goes through a relay with a scoped, short-lived token (§3.1 of
`02-architecture.md`), streamed and resumable by chunk, so a 40 GB world upload never traverses the
control plane and never needs the node to accept a connection. The first version said
"direct-to-node", which a node with no inbound port cannot accept (finding S19). All the interesting
failures here are on the server side of the boundary, and §3 of `06-storage.md` and `17-security.md`
are where they are addressed, the interface's own obligation is simply never to become a privileged
path.

**Metering.** Numbers people will be billed on. Every figure names its source, its window, and its
collection method, and disagreements between the instantaneous and the accounted value are shown
rather than reconciled behind the scenes. `15-observability.md` defines what is measured; the
interface's rule is that a billable number is never rendered without provenance.

---

## 9. Mobile and reach

Hosting customers restart servers from phones, and community moderators do it during an incident.
Everything except large file operations must work on a 400 px viewport, not through a separate app,
which would be a second surface with a second set of gaps, but because the layout was built for it.

Keyboard navigation is complete. The console is the genuinely hard accessibility problem, a live
region that streams thousands of lines is hostile to a screen reader, and is solved with an
explicit paused-and-buffered reading mode rather than by leaving it unsolved.

---

## 10. Open questions

1. **Framework.** Deferred deliberately; the constraints are already fixed, static output, no
   server runtime, streams over polling, and small enough that a host serving it from a cheap CDN
   is ordinary. → here
2. **Custom dashboards.** Hosts want a landing screen composed for their customers. Whether that is
   an extension slot, a saved layout object, or both determines whether it can be per-organization.
   → `16-extensions.md`
3. **Offline and PWA.** Attractive for the incident case, the panel loading while the control plane
   is degraded. Requires deciding what a cached view is allowed to claim, which collides with §2. →
   here
4. **Internationalization.** Community translation is one of the reasons Pterodactyl spread. The
   decision is whether recipe-supplied strings (§5 of `09-recipes.md`) participate in the same
   translation pipeline as core strings, which is not obvious. → `09-recipes.md`
5. **Bulk selection.** The interface will want "select 200 workloads and restart them", which needs
   the bulk mutation semantics still open in §12 of `10-api.md` and the `Operation` object of
   `05-scheduling.md`. → `05-scheduling.md`
