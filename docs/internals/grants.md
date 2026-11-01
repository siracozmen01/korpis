# Grants

Authority in Korpis is one primitive, the grant: a subject, actions, a scope and conditions. There
are no roles. How grants reach people outside Korpis is on
[external identity](external-identity.md); their wire form and share links are on
[capability tokens](capability-tokens.md).

No other product in this market has shipped capability-based authorization in a hosting panel;
TCAdmin, Multicraft, Pelican, AMP and Cloudron all use roles. That is the risk, and it is accepted
because roles cannot express "let this person restart this one server for 24 hours, without an
account". The mitigation is that [templates](#templates) behave like roles in the interface, so if
the pure model proves unusable, the recovery is an interface change, not a schema change.

## Contents

- [Purely additive](#purely-additive)
- [Attenuation](#attenuation)
- [Who may delegate](#who-may-delegate)
- [Scope containment](#scope-containment)
- [Selector scopes](#selector-scopes)
- [Lifecycle and revocation](#lifecycle-and-revocation)
- [Approval](#approval)
- [A token that holds one grant](#a-token-that-holds-one-grant)
- [Templates](#templates)

## Purely additive

A request is a subject, an action and a resource, and it is permitted if any valid chain of grants
permits it. There are no denies, no precedence, no ordering and no priorities. "Allow everything
except delete" becomes "do not grant delete". Deny rules are where authorization becomes
unpredictable: the answer depends on evaluation order, specificity and inheritance, and "why was I
denied" becomes a debugging session. Here evaluation is a search for one permitting chain, and a
denial always has the same explanation: no grant you hold permits this. Narrowing a broad grant
means revoking it and issuing narrower ones: more work, but visible and audited.

## Attenuation

A child grant is valid only if all of these hold against its parent:

| Dimension | Rule |
|---|---|
| actions | the child's are a subset of the parent's |
| scope | contained in the parent's ([containment](#scope-containment)) |
| expiry | no later than the parent's |
| network | the child's source ranges lie within the parent's |
| MFA | required if the parent requires it |
| uses | no more than the parent has left |
| approval | at least as strict ([approval](#approval)) |
| stream range | within the parent's; a grant with one carries only console reading |
| delegation | the parent permits it, and the child's policy is no wider |

These are checked at issue and again at every use, because a parent may have been revoked in
between. A grant is not a certificate that keeps working once printed; it is a claim re-derived
each time it is presented. There is no `grant.issue` action: issuing a grant to someone is
attenuating a grant one holds, and needs only that grant's delegation policy.

## Who may delegate

If delegating were inherent in holding a grant, every share-link bearer and every member of a
Discord role could re-delegate without limit. If it needed an action of its own, a moderator
holding only `workload.restart` could not mint a link for a friend. So delegation is a field of the
grant, and it attenuates like the others:

```
delegation
  allowed     may this grant be attenuated into children at all
  max_depth   how many further levels may exist beneath it
  actions     which of its actions a child may carry
```

A child may be made only if the parent allows delegation with depth at least 1; its actions lie
within the parent's delegable actions; it may itself delegate only if the parent's depth is at
least 2; its depth is at most the parent's minus one; and its delegable actions lie within its own
actions and the parent's delegable ones. Because the policy is in every link and narrows
monotonically, an offline verifier can check it from the chain alone.

| Grant | Default delegation |
|---|---|
| the root grant made at installation | allowed, unbounded, every action |
| a grant from a template | what the template declares; a template must declare it |
| a grant to a set (a Discord role, an OIDC group) | not allowed, unless the binding's template says so |
| a link-form grant | never, and not overridable: a bearer credential mints no children |
| a grant held by an extension | not allowed, unless its manifest declares it and the installer approves |

The moderator case: the `@moderator` role's template grants `workload.restart` with delegation
`{allowed, max_depth: 1, actions: [workload.restart]}`. A moderator can mint a link; the link can
mint nothing.

## Scope containment

```
volume, endpoint, stream   within their workload
workload                   within its project
project                    within its organization
node                       within the organization it was enrolled into
organization               within any ancestor organization
selector(P, expression)    within P, for any expression
```

A node belongs to the organization it was enrolled into, so a host's root grant covers its fleet
and a customer who enrolls their own machine holds it through their own organization.

**Private actions do not cross an organization boundary implicitly.** A host must not read its
customers' consoles, files or secrets by default, yet every grant traces to a root and the host's
root grant contains every action over the root organization. So, as a refinement of containment
rather than a deny: for a private action on a resource owned by organization O, a permitting
grant's scope must be O or within O. A grant scoped to a strict ancestor does not reach O's
private data, however broad. The private actions are console read and write, exec, file read and
write, secret read, backup browse, read and restore, and reads of console and log streams.
Everything else (reading state, metering, stopping, deleting, suspending) still flows down, because
a host must operate the capacity it sells. For a support case, the host mints itself a grant
scoped to the customer's organization, which its root grant permits; that issuance is an effect
the tenant sees, like every operator access to their data. The gateway evaluates this with what it
already holds: the resource's owning organization and the grant's scope.

## Selector scopes

Granting over a label selector ("restart anything tagged `tier=staging` in this project") beats
listing identifiers, and it is dangerous because the set changes after issue: label a production
workload `tier=staging` by accident and the grant covers it. Two rules make it safe, and both are
needed. A selector is always bounded by an explicit container, so a selector grant never escapes
the subtree it was issued in. And a selector grant never confers `label.write` on what it selects,
because changing labels is what widens it; an authorization system that can widen itself
bootstraps its own privileges. So whoever can move a workload into a selector's reach already had
authority over that workload directly.

## Lifecycle and revocation

```
issued --use--> issued
  expires_at reached     -> expired
  max_uses reached       -> exhausted
  revoked                -> revoked, and its whole subtree with it
  root binding unverified -> suspended, and its subtree, reversibly
```

**Revocation is derived.** `revoked_at` is written on the revoked grant and nowhere else, and
validity is computed by walking the chain, which authorization does at every use anyway. So
revocation is instantaneous and costs one row, however deep the subtree. Writing it onto every
descendant would make a deep subtree one enormous transaction or an asynchronous job, and then
"immediately" would be false. The price is in audit: when a descendant stopped being valid is
computed, so the effect log records the revocation that caused it, and a query about a descendant
resolves through the chain to that event. The one place revocation is not instantaneous is a
cached capability token at the edge, bounded on [capability tokens](capability-tokens.md).

Suspension exists because an external identity's assertion is re-checked, not owned. An identity
provider having a bad afternoon should not destroy delegations a person would rebuild by hand, so
a grant rooted in it is suspended, reversibly; organization suspension uses the same state.

**Who may revoke.** A grant may be revoked by its subject; by any subject holding a grant above it
on its chain, since whoever delegated can withdraw; and by any holder of `grant.revoke` on a scope
containing its scope, which is how a host kills a leaked link minted three levels down in a
customer's tree. Inside another organization that is an operator act on tenant authority, visible
to the tenant. `grant.read` on a scope lists the grants within it, bearer links included (by
identifier, issuer, chain, conditions and use count, never the token), so "who can restart this
server" has an answer. For a grant held by a set, the answer names the set and its last verified
membership, dated.

**Provenance, never authority.** A grant from a template records the template and version it came
from. Authorization never reads it, so editing a template widens nothing. Tooling uses it for what
operators ask: "give every Moderator `ext.rcon.kick`" finds those grants and becomes an operation
that issues the additions, as a plan someone approves. Listing grants by template returns each
with its version, limited to what the caller may read.

## Approval

A grant with `requires_approval_by: S` can propose, not apply: a plan made under it waits until
approved. Reading under it is not proposing: actions that change nothing (the reads and backup
browsing) are permitted outright, decided from the action name, because approval governs change.

- An approver for S holds `plan.approve` through a grant whose scope contains S, so approvers get
  fewer and more senior as S grows.
- At least as strict means equal or larger: a child's approval scope must contain the parent's, so
  approval moves up the tree, never down toward the child's own holder.
- Nobody approves their own plan: the approver must be another subject, and the approving grant's
  chain must not pass through the proposing grant, so minting oneself a child makes no second
  approver.
- A plan permitted outright that waits only because it is disruptive or irreversible needs
  confirmation, which its proposer may give.
- The approval is an effect naming both subjects and both chains, and approval requirements are
  never cached.

## A token that holds one grant

Issuing a grant may name a new token instead of a holder: the control plane creates an API token
principal in the same transaction, gives it the grant as its only authority, names it by the
grant's label (then required), expires it with the grant, and answers its secret once. The secret
is stored as a digest, and a replayed issue answers without it. This is how a CI pipeline gets
authority. A link is the wrong shape for that: it is spent by use and can never delegate, while a
pipeline makes hundreds of calls and may pass narrower authority on.

Every grant may carry a label, free text up to 200 characters, shown where it is listed and
recorded in its effect; no decision reads it. When an issuer names no parent, the control plane
checks the attenuation against each grant the issuer holds; exactly one valid parent is used, and
several are refused, because the parent decides what revocation cascades to and a guess would hide
that.

**What it costs.** A token is a bearer secret with no second factor, bounded by its grant rather
than protected by anything about its holder; source ranges narrow where it works. Authentication is
a store read on every request. A lost secret cannot be shown again: the grant is revoked and another
issued, and the dead token remains as a principal holding nothing.

## Templates

```
GrantTemplate
  organization   the owner; offered to its whole subtree
  name           "Moderator", "Billing integration", "Read-only auditor"
  version        1 when made, incremented on every edit; each version kept
  actions
  scope_pattern  organization | project | workload | any, with an optional label selector
                 below an organization or project
  conditions     lifetime, source ranges, MFA, max uses: defaults, overridable at issue
  delegation     required
  retired_at     retired templates are offered nowhere and kept for provenance
```

Applying a template issues ordinary grants, which then attenuate, expire, revoke and audit like
any other. No authorization decision reads the template, so editing it never silently widens
authority already handed out, which in every role-based system is a reliable source of accidents.
There is one evaluation path.

Applying a template is attenuating a grant one holds, with the template filling in the request.
The issuer names the template, the version they read, the holder and the object; the control plane
expands the pattern at that object into a scope, takes actions and delegation from the version and
conditions unless overridden, and runs the ordinary issue against the issuer's own grant. So a
template grants nobody anything: one naming `workload.exec` is applied only by someone who may pass
`workload.exec` on there. A template edited since the version named is a conflict, not a silent
substitution. The control plane checks the pattern at issue, holding the version and the object's
ancestry: the object must lie in the owner's subtree, its kind must match, and a selector needs an
organization or project to look below. After issue, the grant holds a scope and the pattern is
never evaluated again. Conditions are relative (a lifetime, not an expiry), because a template
outlives any instant; an issuer gives absolute ones at issue. An override may loosen a default as
well as tighten it, since the issuer's own grant is the bound, but never changes actions,
delegation or the scope's shape, which are what make a grant a Moderator's.

Making, editing and retiring a template takes `template.write` on its organization; it carries no
authority to raise, so an organization's own administrator may define its roles. Reading the
templates offered at an object takes reading the object, and returns those of every organization at
or above it, so a reseller's customer sees the reseller's templates without reading the reseller.
Templates are retired, never deleted, because grants name their versions.

**What it costs.** A template is not a role anyone can edit into effect: changing what every
Moderator holds is a second issue to each, findable by provenance and reviewable as an operation.
And a reseller's template names are visible to every organization beneath it; a name that should
not be seen below is defined lower down.
