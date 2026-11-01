# Branding

Themes, landing layouts and the source notice. White-labelling is a paid feature elsewhere in this
market; here it is core, complete and free, because no feature is gated by edition. Branding is data
resolved per organization, never patched files: Pterodactyl's ecosystem standardized on
`sed`-patching core files because there was no supported way to change anything, and a reseller's
customers here see the reseller's brand without anyone modifying a deployed file. Extensions
contribute UI through declared slots ([extensions](extensions.md)), never by editing the bundle.

## Contents

- [The source notice](#the-source-notice)
- [What a theme is](#what-a-theme-is)
- [Whose theme a viewer wears](#whose-theme-a-viewer-wears)
- [A stylesheet is fenced](#a-stylesheet-is-fenced)
- [How the panel applies it](#how-the-panel-applies-it)
- [Landing layouts](#landing-layouts)
- [What it costs and does not do](#what-it-costs-and-does-not-do)

## The source notice

The panel's JavaScript is conveyed to every browser that loads it, and AGPL-3.0 attaches its source
obligation to that. So white-labelling stops at one fixed path: every build serves `/about/source`,
naming the exact revision it was built from and where that source is published, and no theme can
remove or restyle it. It is not in the branded chrome; a host's customers see the host's brand
everywhere they look, and the notice is where the licence needs it.

The control plane answers `/about/source` on its address whether or not it serves a panel, unstyled
and under a policy that runs no script and loads no style, naming its own version, revision and
source, and the panel's when the bundle in `--web-root` carries the `about/source.json` its build
writes. The bundle also carries `about/source/index.html`, generated at build time from the revision
and the repository the build names, so a panel served from a CDN meets the licence with no control
plane in front of it. A host that modifies the panel and builds it names its own repository, which
is the obligation the AGPL places on it.

## What a theme is

A `Theme` belongs to an organization and is inherited by everything beneath it unless a descendant
sets its own. It holds:

| Field | Bound |
|---|---|
| product name | at most 64 characters, one line |
| logo | PNG, WebP or SVG, matching its media type, at most 256 KiB |
| colours | only the panel's nine tokens, only as `#rrggbb`, for the light and the dark scheme |
| sign-in text | at most 2,000 characters |
| stylesheet | optional, at most 64 KiB, no `@import` and no backslash escape |

The escape is refused because an escape is how CSS spells `@import` past a check. Every field is
checked where it is set and again where it is read back. A colour is refused where it is set if text
on the background or on a surface, muted text or a danger notice on the background, or text on the
accent would fall below WCAG's 4.5:1, each token the theme leaves unset taken as the panel's own (a
test holds that table to the panel's stylesheet).

`SetTheme` on the organization service, under `org.write`, sets or removes an organization's own
theme against the version `GetTheme` returned, each change an effect `org.theme.set`. `GetTheme`
answers for an organization, project or workload under that object's read action, with the theme of
the nearest ancestor that set one and which organization it came from; through a share link it
spends no use, since the page it styles spent the one it costs. The logo is inside the theme, so the
panel fetches nothing a theme names. `korpis theme show|set|clear` is the command.

## Whose theme a viewer wears

A theme is a provider's brand for the people it serves, so it follows the viewer, not the object.
The control plane decides, since it holds the viewer's grants:

- **At a [portal](portals.md)**, the portal's organization's, before anyone signs in and after,
  whatever the viewer reads. Which portal a request is for is decided by the connection, whose
  certificate named it, never by anything the request says, such as its `Origin`. `GetServerInfo`
  names the portal and its organization so the panel knows where it is without guessing from its
  own address.
- **Elsewhere, signed in**, `GetLandingTheme` answers that of the nearest organization above
  everything the viewer's grants reach, beneath the organization a confined session is confined to;
  with no grant, the installation root's. A customer holding only a project's grant reads no
  organization at all, which is why the panel does not find the provider from what it lists.
- **Signed out**, `GetSignInTheme` answers anyone with the installation root's theme (the first
  organization made with no parent), charged to the caller's address like every call made without a
  credential ([rate limits](rate-limits.md)), and at a portal the portal organization's.

If the theme followed the object, a tenant could dress its provider's screens: a host operator
opening a customer's workload would wear the customer's stylesheet, which can hide a warning or
restyle a destructive button on the operator's own panel. The cost is that a host's operator never
sees a customer's brand while working inside the customer's organization, and previews one only
through the editor.

## A stylesheet is fenced

A stylesheet is code that runs in the panel. The panel's content security policy, carried in the
page itself so it holds wherever the bundle is served, admits images, fonts and connections from the
panel's origin, the control plane and its relays, and nothing else, so a stylesheet cannot send what
it sees to its author: a selector matching a value on screen and loading a background image from
elsewhere is refused by the browser. A host wanting a font or image from its own CDN serves it
through the theme's own data instead. What a stylesheet can still do is restyle what the viewer
sees, which a reseller styling its customers' panel is meant to; it cannot reach `/about/source`,
which is unstyled.

Because a rule can hide, move or replace any text, the stylesheet is never worn where what the page
says is the viewer's own business rather than the brand's: the profile, with its passwords, passkeys
and sessions, and a share link's page, whose holder is told what the link allows, wear the theme's
colours, name and logo alone, and the device approval wears no theme at all. Email templates join a
theme when Korpis sends email, which it does not: there is no emailed reset, and invitations are
handed over by whoever issues them.

## How the panel applies it

`web/src/theme`. A signed-in viewer wears the `GetLandingTheme` answer on every page, except the
profile and a share link's page, which wear `GetTheme` for the place they open without its
stylesheet; the sign-in and invitation pages wear the sign-in theme. The colours become the panel's
custom properties, the light scheme on the root and the dark in its media query, and only a
`#rrggbb` value of one of the nine tokens is written, whatever the answer held; the stylesheet
follows. Both are constructed stylesheets the page adopts rather than loads or inlines, so the
policy keeps `style-src 'self'` with no inline style, and a constructed sheet ignores `@import`
besides. The name is the page's title and stands with the logo in the chrome; the logo is an image
from a `data:` address, which the policy admits for images alone and where an SVG runs nothing; the
sign-in text is shown as text. A test reads the policy out of the page and fails if it ever admits a
script or style from elsewhere, or anything `unsafe`.

The organization's page offers the theme for editing to whoever opens it, the control plane deciding
who may save: each field, a preview worn on the page before saving, a save against the version read,
and removing the organization's own theme so the nearest above applies again.

## Landing layouts

A host composes the landing screen for its customers as a `Layout`: data, so per organization by
construction, inherited by the subtree unless a descendant sets its own, and able to place only what
already exists. It is a heading of at most 64 characters on one line and at most 24 items in order:

| Item | What it is |
|---|---|
| `places` | what the viewer's grants open; placed exactly once |
| `quota` | the quota of the viewer's top organization as the viewer reads it; at most once, empty for a viewer with no one top organization |
| `notice` | a heading of at most 120 characters and a text of at most 2,000, shown as text, never markup |
| `link` | a label of at most 64 characters and an `https` address with a host and no user information, at most 2,048 bytes, shown with its host |
| slot | an extension's slot by name; refused while no extension declares slots |

`places` is required because a landing screen is how a viewer reaches what their grants open: a host
may arrange what its customers see first, not hide what they can reach. The slot kind is in the
format already so a layout made now is the same object once slots arrive.

The layout a viewer lands on is chosen exactly as the theme is. `SetLayout` is `org.write` against
the version `GetLayout` returned, each change an effect `org.layout.set`; `GetLayout` answers under
the read action of the object asked about; `GetLandingLayout` answers any caller signed in, though
they may read none of the organizations above them, since every component renders under the caller's
own authority and a provider's notices are addressed to everyone it serves. A share link's holder
lands nowhere, the link opening one place.

The panel (`web/src/layout`) shows the heading and items in order, and its own landing where there
is no layout or it cannot be read; a link opens in a new window with no reference back. The editor
on the organization's page moves items, removes any but `places`, adds notices and links, adds the
quota once, and saves against the version read or removes the layout. `korpis layout` is the
command.

## What it costs and does not do

A layout composes the landing screen and nothing else; every other page is the panel's. A notice or
link says what its organization says, which the panel does not vouch for, which is why a link's host
is shown beside its label. Neither a theme nor a layout can narrow or widen what a viewer sees,
since every component reads with the viewer's credential. Each is one more object per organization,
resolved by the same walk up the tree, and one more read when the landing screen opens (another for
the quota where placed). A reseller's sign-in page at its own address needs a portal.
