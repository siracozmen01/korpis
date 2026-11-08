# Extensions

How Korpis is extended: what an extension is, how it is installed with grants, who may install it,
how its contract is versioned, and what it can never do. How Korpis calls an extension, and each
provider type, is on [providers](providers.md).

## Contents

- [Why it is not optional](#why-it-is-not-optional)
- [Core uses the same mechanism](#core-uses-the-same-mechanism)
- [An extension is a workload](#an-extension-is-a-workload)
- [What an extension is made of](#what-an-extension-is-made-of)
- [Installing grants it what it needs](#installing-grants-it-what-it-needs)
- [Uninstalling and upgrading](#uninstalling-and-upgrading)
- [Who may install one](#who-may-install-one)
- [Versioning](#versioning)
- [What extensions cannot do](#what-extensions-cannot-do)
- [Distribution](#distribution)

## Why it is not optional

Pterodactyl has no extension API, and the ecosystem that grew anyway standardized on patching core
PHP files with `sed`: it breaks on every upgrade, cannot be audited or cleanly uninstalled, and
makes the panel's own releases a hazard. That is what people do when the alternative is not
existing. The interface has to exist before its consumers, because after them the patching
convention is already load-bearing.

## Core uses the same mechanism

If the extension interface is not sufficient for a core feature, the interface is fixed, not the
exception granted. These are extensions, not core code with an API bolted beside it:

| Extension | Point |
|---|---|
| game query protocols (Source, Minecraft, GameSpy) | health provider |
| RCON, console adapters for non-stdout consoles | console provider |
| Steam app installation | recipe step provider |
| DNS providers (Cloudflare, Route53, RFC 2136) | DNS provider |
| object-storage and filesystem backup targets | backup target provider |
| OIDC, SAML, LDAP, Discord identity sources | identity provider, in the trust base |
| protocol-aware routers | edge router provider |
| BGP speakers, a provider's floating addresses | address router provider |
| Ceph RBD, an iSCSI target's management interface | block storage provider |
| alert routing beyond the built-in minimum | event consumer |
| the Discord client and the chat adapter interface behind it | actions, events, streams |

Runtime drivers are deliberately not extensions: a driver's privilege is different in kind, and they
are processes of their own behind their own interface ([runtimes](runtimes.md)).

## An extension is a workload

An extension is an OCI artifact, distributed and signed exactly as a recipe is, and runs as a
workload on a node, with an isolation tier, a reservation, declared egress and a lease. It has a
subject and holds grants. It cannot read the database, open a socket the control plane trusts
implicitly, or touch a Korpis file. So it gets the platform's isolation, scheduling, observability,
quota and audit for free, a misbehaving extension is a misbehaving workload, which the system
already contains, throttles, restarts and evicts, and "can an extension take down my control plane"
has the answer "can a tenant's game server take down my node" has.

**It proves who it is as every workload does**: its node vouches for it through the identity socket,
and the control plane authenticates it as `extension:<id>`, the subject its grants were issued to.
It holds no secret outliving its placement, and an uninstalled extension's workload obtains nothing.

## What an extension is made of

An extension is a recipe with one more section: its artifact has the recipe layout under type
`application/vnd.korpis.extension.v1`, its document `kind` is `Extension`, everything a recipe
document holds describes its workload, and the `extension` block describes the extension:

```
extension:
  api_version   korpis.extension.v1     the contract it speaks
  requires      []capability            contract capabilities it cannot work without
  needs         []{actions, scope, reason}   scope: target | organization | self
  egress        []host:port             its workload's egress policy
  subscribes    []{kinds, min_severity, workloads}
  provides      []{type, name, arguments}
  actions, ui_slots
```

A need's `reason` is required, because it is the line the installer reads before approving; an
action is needed over a scope once. A subscription has no projects, which the installation narrows,
since a document cannot know the organization it will be installed into, and a document that
subscribes must need `event.read`, or it would install cleanly and deliver nothing forever. A
`provides` type is one of `dns`, `backup_target`, `step`, `health`, `console`, `identity`,
`edge_router`, `chat_adapter`, `key_management`, `address_router`, `block_storage`, its name a
lowercase word, or for a step or health protocol `<publisher>/<name>` with dotted lowercase words,
so two publishers' names cannot collide. The extension's name is a lowercase word, since it names
its actions (`ext.<name>.<action>`). A `Recipe` with an extension block, or an `Extension` without
one, does not load. One format means one signing path and one trust policy; the cost is that an
extension author writes a recipe, which every workload author already does.

## Installing grants it what it needs

An extension declares what it needs, the installer reads and approves it, and from then on it acts
as itself in the audit, under grants that attenuate like everyone else's: the phone-application
permission model, which works here because grants already exist. A user sees what an extension may
do by reading its grants, revokes one without uninstalling, and reads every action it took in the
effect log.

An install names the artifact, the organization or project it is installed into, the project its
workload runs in, and the needs approved. A dry run resolves the artifact under the trust policy,
checks where it goes and who may install it, and answers with the manifest and the grants it would
issue. The real install requires `approved_needs` to equal the document's needs exactly, in order,
with their reasons: an approval is of the list the installer read, never one rebuilt from it. A need
over `target` is granted on the project installed into, else the organization; over `organization`,
the organization; over `self`, the extension's own workload, which the same transaction creates.
Each grant is attenuated from the first of the installer's grants the attenuation check accepts, so
an installer whose grants may not be delegated can install nothing that needs anything; a need no
grant of the installer's covers is refused by name, before anything is created. The workload, its
plan, every grant, the installation and their effects (`grant.issued`, `extension.install`) commit
together; its `provides` entries become registrations its streams may claim, and an entry another
live installation in the organization already serves refuses the install (`provider.taken`). An
installation's status is its workload's, observed.

An extension can never hold authority its installer did not, because a grant only yields weaker
children: a reseller installing one in its own organization cannot reach a sibling's, which needs no
check.

**An extension's artifact is installed, never declared.** A declaration naming an extension's
artifact is refused, as is one giving an extension's workload any digest but the one installed or
upgraded to: the same code run as a plain workload would hold none of its grants and fail in ways
nobody could diagnose, and a new version declared directly could arrive with needs nobody approved.
Its configuration, power and placement are declared as any workload's.

## Uninstalling and upgrading

Uninstalling needs `extension.uninstall` there and `workload.delete` on its workload, whose
retirement it proposes. One transaction revokes every grant the installation holds, and everything
descended from them, ends its subscriptions and registrations, closing their streams, and proposes
the retirement, which waits for a person. Meanwhile the workload runs holding no grant, and a
credential its node obtains authenticates as `workload:<id>`. The installation's record stays, since
effects name its subject as long as the audit is kept; its name is free again where it was
installed.

`ExtensionService.UpgradeExtension` takes the installation and a tag or digest of the same artifact;
another extension's, or the digest installed, is refused. The upgrader needs what installing needed,
the new version must pass the versioning check, and the approved needs must be the new document's
exactly. A held grant whose actions and object a new need still asks for is kept, keeping its place
in the grant tree; a need no held grant serves is granted from the upgrader's own grants; a grant no
need asks for is revoked with its descendants. Kept `provides` entries keep their registrations, so
old replicas serve until replaced, the drain of [providers](providers.md#replacing-a-provider);
dropped ones end, their calls in flight failing as retryable. Unchanged subscriptions keep their
position; new ones start at the end of the log. The new digest is declared on the same workload with
its current configuration, keeping the subject `extension:<id>` and everything recorded against it.
All of it is one transaction, refused with `extension.changed` if the installation changed since
read.

## Who may install one

With operator, reseller and customer, "the operator approves" names three people, and an approval
object is the wrong instinct. "It runs code on someone else's machine" proves too much: tenants
already run arbitrary code, which is the product, and an extension is a workload in the same tier,
quota and egress policy. The real distinction is the direction of the call: **an extension Korpis
calls is infrastructure; an extension that calls Korpis is a tenant.**

| Kind | Whose exposure | Who may install |
|---|---|---|
| event consumer, action, UI slot | the installer's own data and quota | the installer; its grants suffice |
| DNS, backup target, edge router, address router, block storage | the control path: Korpis calls it and waits on it | whoever owns the resource it provides for |
| identity | authentication itself: whatever it asserts is believed | whoever holds authority over the whole organization it authenticates into |
| step, health, console | the install path and probes of every workload whose recipe names them | whoever holds authority over the organization whose recipes may use them |

So installing anything that provides needs `extension.install` over the organization as well as
where it is installed, since a provider serves its organization's subtree, and an identity provider
is installed into the organization itself. A reseller runs a DNS provider for its own zones and
cannot place code in the operator's forwarding path. Tenant-direction extensions need nothing new:
their authority is bounded structurally, their consumption metered against the installer, and their
egress the tenant's egress policy, so an operator forbidding arbitrary egress has already stopped
one reaching Cloudflare.

**An operator's rule no tenant loosens.** A trust policy is an organization's own and a child may
accept what its parent does not, which is right for what a tenant runs; an operator restricting
extensions needs a rule beneath which nobody widens it. So a root organization keeps an extension
policy (`ExtensionService.GetExtensionPolicy` under `org.read` there, `SetExtensionPolicy` under
`org.write` by compare-and-swap, each change an `extension.policy.set` effect) that governs every
install and upgrade beneath it, dry runs included, before anything else of the extension is believed:
the registries an extension's artifact may come from (a host, or a repository prefix beneath one,
matched segment by segment), the publishers one of whom must have signed its digest (cosign keys,
checked as a trust policy's signers are, on top of the installing organization's own policy), and
whether anyone without `extension.install` over the root may install one at all, which a shared host
not wanting customers to install extensions turns off. All open is the default. A refusal is
`extension.not_allowed` and names the rule. The policy changes nothing already installed: removing a
registry stops its extensions being installed or upgraded, and uninstalling is an action on each.

**Providers are named, scoped and pinned.** A registration belongs to an organization and is visible
to its subtree; a child organization's own shadows its parent's for its subtree only, visibly. When
a recipe is declared, each step kind and query protocol it uses is resolved to the nearest
organization with a live installation providing it, its arguments are checked against that
installation's declared schema, and the installation and its artifact digest are pinned in the
intent, set by the control plane whatever a client sends, so a workload never changes provider
because someone registered a new one. A changed pin is an install change. Upgrading a step provider
changes its digest, so every pinned workload must be declared again before its install resolves,
failing until then and saying so: the lockfile's promise kept.

## Versioning

The API's compatibility rules apply unchanged: additive only, no renames or splits, capabilities
negotiated. An extension declares its contract version and required capabilities, and
**incompatibility is refused at install, a dry run included, naming what is missing**, never
discovered at first use six weeks later during an incident. The support window is every contract
version shipped by a supported release, every LTS included, since an upgrade silently orphaning an
operator's DNS provider is an outage the operator did not choose.

The contract is `korpis.extension.v1`, and a capability names an interface the control plane
actually speaks, never one it merely tolerates: `events.subscribe`, `identity.credential`,
`provider.serve`, `provider.action` for serving its own actions, and one `provider.<type>` per
provider type whose calls are in the contract and the SDK. A type whose calls are not yet in the contract (console, edge router, chat adapter) is not
offered, and an extension requiring it is refused, since it would wait for calls nothing makes. A
refusal is `extension.incompatible`, naming what is missing and what is offered; a client reads both
from `ServerInfo` (`extension_contracts`, `capabilities`).

A type grows by addition alone. Identity providers able to sign people in say so when registering
(`sign_in`) and answer `Authorize` with an `https` address (at most 8 KiB, since RFC 6749 section
3.1 requires TLS of an authorization endpoint, else `provider.malformed`), take an authorization
code as `Assert` evidence, and may answer `VerifyMembers` with `unknown` members, whose last
confirmation stands; a provider built before answers no `unknown` and is unchanged. A sign-in's
secrets (state, nonce, code, verifier) are emptied from the stored call as soon as it is answered,
and by the sweep within a minute for a dead caller.

## What extensions cannot do

The boundary is the design:

- read or write the database, or read, modify or replace Korpis's own files;
- hold authority exceeding their installer's;
- receive another subject's capability token: acting for an asserted user goes through a ticket for
  one action on one object, redeemed only beside the extension's own credential;
- act, during an invocation, on a resource the invoker was not authorized for;
- run in the control plane's process, address space or privilege;
- add a field to a core object: they keep their own state, keyed by core identifiers;
- block a reconciliation loop indefinitely;
- disable, weaken or bypass a grant condition, or remove a recipe-declared redaction pattern, which
  matter because an extension is often installed by an operator for tenants, who are not always the
  party being protected.

Extensions compose only through the event stream and the API, never directly, so there is no
dependency graph between them to resolve.

## Distribution

OCI artifacts in ordinary registries, addressed by digest and signed with cosign, as recipes are,
because registries already provide hosting, mirroring, signing and CDN and Korpis is not a
marketplace. Extensions are licensed at their authors' choice, and **paid, proprietary extensions
are explicitly permitted**: no platform tax, and a supported place for a commercial host's
proprietary differentiation. The SDK, `korpis-extension`, is Apache-2.0 like the protocol and links
nothing of Korpis but the wire contract, so an extension carries whatever licence its author
chooses.
