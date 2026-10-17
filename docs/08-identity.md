# Identity, Tenancy, and Authority

**Status:** design **Date:** 2026-08-07 **Depends on:** [`01-model.md`](./01-model.md),
[`02-architecture.md`](./02-architecture.md) **Resolves:** open question 3 of `01-model.md`; 2 of
`02-architecture.md`; 5 of `05-scheduling.md` **Implements:** Bet 1, Bet 4, Principle P6, Rule K-14

---

## 1. The risk in this document

Every other decision in Korpis has a precedent somewhere. This one does not: no product in this
market has shipped capability-based authorization in a hosting panel. TCAdmin, Multicraft, Pelican,
Calagopus, AMP, Cloudron, all use roles, because roles are what everyone knows.

That is the risk and it is accepted, because roles cannot express the operation a chat-native
platform performs constantly: *let this specific person restart this one server for the next 24
hours, without creating an account.* Bet 1 requires it. Bet 4 requires the same machinery pointed
at tenancy.

The mitigation is not confidence. It is that §4 adds a familiar layer on top, templates that behave
like roles in the interface, so if the pure model proves unusable to real operators, the recovery
is a UI change and not a schema change.

---

## 2. Tenancy

```
Organization ──▶ Organization ──▶ Organization ──▶ …
      │                │
      └──▶ Project     └──▶ Project ──▶ Workload
```

Organizations nest without limit. Each holds a `QuotaSet` (§5.1 of `05-scheduling.md`) whose
reservations can never exceed its parent's unallocated reservations, and whose limits may exceed the
parent's only by the overcommit ratio the parent enables. Projects group workloads and are the usual
unit of delegation.

**There is no `Reseller` object, and there never will be.**

| Real role | What it is in Korpis |
|---|---|
| Hosting provider | a root organization |
| Their customer | a child organization with an allocated `QuotaSet` |
| That customer's customers | grandchildren |
| A community's admin team | a project-scoped grant |
| A single game server owner | a workload-scoped grant |

Nothing in the code distinguishes these levels. TCAdmin sells reseller support as a premium feature
because its tenancy was not built for it; here it is what a correct tenancy model produces by
default (Bet 4, Rule K-14).

### 2.1 Suspension is reversible, and it is not revocation

> Second external review, finding S32. Recorded in §17 of `23-walkthroughs.md`.

The most common thing a host does to a customer is not delete them. It is **suspend them for
non-payment and restore them a week later.** The model had no such operation. Suspension existed
only as the consequence of an identity binding failing (§5.1), and the only operator lever was
revocation, which is irreversible and cascades: a host suspending a customer that way would destroy
every moderator grant, share link, and CI token the customer had built, and restoring the customer
would mean the customer rebuilding all of it by hand.

```
Organization
  status        active | suspended
  suspension    {by, at, reason, on_suspend: keep_running | stop}?
```

- `org.suspend` and `org.resume` are actions (§5 of `10-api.md`), and they are only effective from
  **above**: the grant exercising them must be scoped to a strict ancestor of the organization, so a
  tenant cannot resume itself.
- A suspended organization's grants, and those of every organization beneath it, are **suspended**
  in exactly the sense of §3.5: they stop authorizing, they are not destroyed, and resuming restores
  them unchanged. The cascade is derived, like revocation, so suspending a reseller with forty
  customers is one row.
- `grant.read`, `org.read`, and reading the organization's own metering and audit remain available
  to the suspended tenant, because a customer being asked to pay must be able to see what for.
- `on_suspend` decides the workloads: `keep_running` (access suspended, service continues) or `stop`
  (a power plan for every workload, executed by the host's authority and visible to the tenant).
  Volumes and backups are untouched either way; deleting a suspended customer's data is a separate,
  irreversible plan that the host makes on the record.

Billing systems need exactly this lever and nothing else from Korpis here, which is K-12 applied:
Korpis provides the reversible state; deciding when to apply it is the billing system's job.

---

## 3. Grants

### 3.1 Grants are purely additive. There are no deny rules.

A request is `(subject, action, resource)`. It is permitted if **any** valid grant chain permits
it.

No denies, no precedence, no ordering, no priority numbers.

This is a deliberate and slightly uncomfortable stance, because "allow everything except delete"
becomes "do not grant delete" rather than "grant everything, deny delete". The reason is that deny
rules are where authorization systems become unpredictable: once denies exist, the answer to "can I
do this" depends on evaluation order, specificity rules, and inheritance precedence, and the honest
answer to "why was I denied" becomes a debugging session. Every large IAM system has this problem
and none has solved it.

Purely additive means the evaluation is a search for one permitting chain, and the explanation for
a denial is always the same sentence: *no grant you hold permits this.*

Narrowing an existing broad grant means revoking it and issuing narrower ones. That is more work,
and it is visible, auditable work rather than an invisible rule interaction.

### 3.2 Attenuation, precisely

A child grant is valid only if **all** hold against its parent:

| Dimension | Rule |
|---|---|
| actions | `child.actions ⊆ parent.actions` |
| scope | `child.scope ⊑ parent.scope`, contained, per §3.3 |
| expiry | `child.expires_at ≤ parent.expires_at` |
| network | `child.source_cidr ⊆ parent.source_cidr` |
| MFA | `child.requires_mfa ≥ parent.requires_mfa` |
| uses | `child.max_uses ≤ parent.remaining_uses` |
| approval | `child.requires_approval_by` at least as strict, as defined in §3.6 |
| stream range | `child.stream_range ⊆ parent.stream_range`; a grant with one carries only `workload.console.read` (§8 of `14-streams.md`) |
| delegation | the parent permits it, and the child's policy is no wider (below) |

Checked at issue **and re-checked at every use**, because a parent may have been revoked in
between. A grant is not a certificate that keeps working once printed; it is a claim that is
re-derived every time it is presented.

#### Who may delegate is a property of the grant

> Second external review, finding S20. Recorded in §17 of `23-walkthroughs.md`.

The table above said what a child may contain and never said who may create one. Both readings
broke something. If delegating is inherent in holding a grant, every share-link bearer, every
member of a Discord role, and every extension can re-delegate without limit until expiry. If it
requires `grant.issue`, the canonical example of this document (a moderator holding only
`workload.restart` mints a link) is impossible. So delegation is neither ambient nor a separate
action. It is a field of the grant, and it attenuates like every other field:

```
Grant.delegation
  allowed     bool         may this grant be attenuated into children at all
  max_depth   int          how many further levels may exist beneath this grant
  actions     []Action     which of this grant's actions a child may carry; ⊆ actions
```

| Rule | Checked |
|---|---|
| a child may be created from G only if `G.delegation.allowed` and `G.delegation.max_depth ≥ 1` | at issue, by the gateway |
| `child.actions ⊆ G.delegation.actions` | at issue and at use |
| `child.delegation.allowed` requires `G.delegation.max_depth ≥ 2` | at issue and at use |
| `child.delegation.max_depth ≤ G.delegation.max_depth − 1` | at issue and at use |
| `child.delegation.actions ⊆ child.actions ∩ G.delegation.actions` | at issue and at use |

Because the policy is in every link of the chain and narrows monotonically, an **offline**
verifier can check it from the chain alone, which is the property §6 needs.

The defaults are where most of the safety lives:

| Grant | Default delegation |
|---|---|
| a root grant created at installation | allowed, unbounded depth, every action |
| a grant issued from a template | whatever the template declares; templates must declare it |
| a grant to a set subject (a Discord role, an OIDC group) | **not allowed**, unless the binding's template says otherwise |
| **a link-form grant (§6.1)** | **never allowed**, and not overridable: a bearer credential cannot mint children |
| a grant held by an extension | not allowed, unless its install manifest declares it and the installer approves it |

The moderator example now has a definite shape: the `@moderator` role's template grants
`workload.restart` with `delegation: {allowed, max_depth: 1, actions: [workload.restart]}`. A
moderator can mint a link; the link can mint nothing.

There is no `grant.issue` action. Issuing a grant to someone **is** attenuating a grant one holds,
and the only authority it needs is the delegation policy of that grant. Revoking is different,
because it acts on grants one did not issue, and §3.5 says who may.

### 3.3 Scope containment

```
Volume | Endpoint | Stream  ⊑  their Workload
Workload                    ⊑  its Project
Project                     ⊑  its Organization
Node                        ⊑  the Organization it was enrolled into
Organization                ⊑  any ancestor Organization
Selector(P, expr)           ⊑  P            for any expression
```

The last line resolves open question 3 of `01-model.md`.

> Correction, 2026-10-13, from the implementation. The node line was missing. A node had no
> parent, so the only grant that could cover `node.read` or `node.cordon` was one naming that
> node, and the root grant, which "contains every action", contained no node at all. Enrollment
> was already `node.enroll` scoped to one organization (§7 of `02-architecture.md`); the node now
> belongs to that organization (§3.4 of `01-model.md`), so a host's root grant covers its fleet
> and a customer who enrolls a machine of their own holds it through their own organization.

#### Tenant-private actions do not cross an organization boundary implicitly

> Second external review, finding S24. Recorded in §17 of `23-walkthroughs.md`.

§7 promises that a parent organization has no default access to a child's console, files, or
secrets. Containment as written made that unevaluable: every grant traces to a root (§3.2 of
`01-model.md`), the host's root grant contains `workload.file.read` over the root organization, and
containment then covered every customer's files beneath it. No rule existed that any evaluator
could have used to produce the "no" in that table.

The rule is a refinement of containment, not a deny, so §3.1 stands:

```
private actions   workload.console.read · workload.console.write · workload.exec
                  workload.file.read · workload.file.write · workload.secret.read
                  backup.browse · backup.read · backup.restore
                  stream reads of console and log streams
```

**For a private action on a resource owned by organization O, a permitting grant's scope must be
O or contained in O.** A grant scoped to a strict ancestor of O does not reach O's private data,
however broad it is. Everything else (reading state, metering, stopping, deleting, suspending)
continues to flow down through containment as before, because a host must be able to operate the
capacity it sells.

The gateway evaluates it with information it already holds: the resource's owning organization and
the grant's scope. When a host needs a customer's files for a support case, it mints a grant to
itself scoped explicitly to that customer's organization, which its root grant permits by ordinary
attenuation. That issuance is the "explicit grant" of §7, it is an effect, and it is visible to the
tenant like every other operator access to their data.

### 3.4 Selector scopes, and the trap in them

> Resolves open question 3 of `01-model.md`.

Granting over a label selector ("restart anything tagged `tier=staging` in this project") is far
more useful than enumerating IDs. It is also dangerous in a way that is easy to miss: **the set
changes after the grant is issued.** Label a production workload `tier=staging` by accident and the
grant now covers it.

Two rules make selectors safe, and both are necessary:

**Rule 1: a selector is always bounded by an explicit container.** `Selector(project P, expr)`,
never a bare expression. A selector-scoped grant cannot escape the tenancy subtree it was issued
within, whatever anyone labels anything.

**Rule 2: a selector-scoped grant can never confer `label.write` on what it selects.**

Rule 2 is the important one. Changing labels is what widens a selector grant's reach, so if a
selector grant could confer label-writing, it could be used to widen itself, an authorization
system that can bootstrap its own privileges. Under Rule 2, `label.write` is only ever conferred by
a grant scoped explicitly, by ID or by container. Whoever can move a workload into a selector's
reach is necessarily someone who already had authority over that workload directly.

A selector grant can therefore only ever be widened by someone who did not need it widened.

### 3.5 Lifecycle and revocation

```
issued ──use──▶ issued
   ├── expires_at reached ──▶ expired
   ├── max_uses reached ────▶ exhausted
   ├── revoked ─────────────▶ revoked  ⟶ cascades to entire subtree
   ├── parent revoked ──────▶ revoked
   └── root binding unverified ▶ suspended ⟶ cascades, reversible (§5.1)
```

Revocation cascades transitively and takes effect immediately in the control plane. The one place
it is not instantaneous is a cached capability token at the edge, bounded and quantified in §6.

**Revocation is derived, not written to the subtree.**

> External review, finding R4.

"Cascades transitively and immediately" describes two different implementations and the document
implied both. Writing `revoked_at` onto every descendant makes a deep subtree either one enormous
transaction or an asynchronous job, and if it is asynchronous then "immediately" is false.

So the cascade is **derived**: `revoked_at` is written on the grant that was revoked and nowhere
else, and validity is computed by walking the chain, which authorization already does at use time
("re-checked at use time, because a parent may have been revoked in between", §3 of `01-model.md`).
Revocation is therefore genuinely instantaneous and costs one row.

The audit consequence has to be stated, because it is the price. "When did this grant stop being
valid" is a computed answer rather than a stored one, so the `Effect` log records the **revocation
that caused it**, and a query about a descendant resolves through the chain to that event. An
`Effect` still names the grant it acted under (§3.3 of `01-model.md`); what it does not do is claim
that grant was independently revoked when its parent was.

`suspended` exists because one root of a grant tree is not a row in this database. A grant rooted
in an `ExternalIdentity` depends on an assertion Korpis re-checks rather than owns, and §5.1 gives
that re-check an interval and a cascade. Suspension is reversible and revocation is not, which is
the right asymmetry: an identity provider having a bad afternoon should not destroy delegations
that a human will have to rebuild by hand. The same state is what organization suspension uses
(§2.1).

#### Who may revoke, and who may see

> Second external review, finding S20.

Revoking acts on a grant someone else may have issued, so it needs its own answer. A grant G may be
revoked by:

- its **subject**, relinquishing it;
- **any subject holding a grant on G's chain above it**, its issuer and every ancestor, because
  whoever delegated authority can always withdraw it;
- **any holder of `grant.revoke` on a scope containing G's scope**, which is how a host kills a
  leaked share link minted three levels down in a customer's tree without being on its chain. For
  a grant inside another organization this is an operator action on tenant authority, so it is
  visible to that tenant (§7).

`grant.read` on a scope lists the grants whose scope it contains, including bearer links (by
identifier, issuer, chain, conditions, and use count, never the token itself), so "who can restart
this server" is a query with an answer. For a grant held by a set subject the answer names the set
and its last verified membership, dated, because Korpis knows membership only as of its last
verification (§5.1) and says so rather than presenting it as current.

**Grants remember the template they came from, as provenance and never as authority.** A grant
issued from a template records `from_template: id@version`. Authorization never reads the field,
so editing a template still widens nothing (§4). What the field enables is the operation operators
actually ask for: "give every Moderator `ext.rcon.kick`" finds the grants issued from that template
and produces an `Operation` that issues the additional grants, as a plan someone approves, rather
than silently changing what an existing grant means.

### 3.6 Approval

> Second external review, finding S27. Recorded in §17 of `23-walkthroughs.md`.

`requires_approval_by` was a `Scope` with an attenuation rule of "at least as strict", and scopes
are ordered by containment, not by strictness. The natural reading let a delegate point approval at
a project they controlled and approve their own requests. The definitions:

- **A grant with `requires_approval_by: S` can propose, not apply.** A plan proposed under it waits
  in `proposed` until approved.
  Reading under it is not proposing: an action that changes nothing (the `*.read` actions and
  `backup.browse`) is permitted outright, because approval governs change (P5), and a proposer who
  could not read would not see the plan it proposed or the workload it is for. Every other action,
  an extension's included, is a change and is proposed. Evaluation decides this from the action
  name alone (clarified 2026-10-13, from the implementation).
- **An approver for S is a subject holding `plan.approve` through a grant whose scope contains S.**
  The set of approvers therefore shrinks as S grows: approval scoped to an organization can be given
  only by someone with authority over the whole organization.
- **"At least as strict" means equal or larger.** `child.requires_approval_by` must be set when the
  parent's is, and must contain the parent's: a child can move approval up the tree toward fewer,
  more senior approvers, never down toward its own holder.
- **Nobody approves their own plan.** The approving subject must differ from the proposing subject,
  and the approving grant's chain must not pass through the proposing grant, so minting oneself a
  child grant does not manufacture a second approver.
  This governs approval of a plan proposed under `requires_approval_by`. A plan whose proposer was
  permitted the action outright, and which waits only because it is disruptive or irreversible,
  needs **confirmation**, which its proposer may give (§3.3 of `01-model.md`, clarified
  2026-10-12).
- The approval is an effect naming both subjects and both chains, and `requires_approval_by` is
  never cached (§5.1 of `02-architecture.md`).

### 3.7 A token issued to hold one grant

> Added 2026-10-13, from the implementation of `korpis grant issue` (§7 of `13-surface-cli.md`).
> The model had a `Token` subject "for a CI pipeline" (§3.2 of `01-model.md`) and no way for one
> to come into existence except installation; the CLI's flagship automation example could not be
> carried out.

**Decision.** Issuing a grant may name, instead of a holder, a **new token**: the control plane
creates an API token principal in the same transaction as the grant, gives it the grant as its only
authority, names it by the grant's label (which is then required), expires it with the grant, and
answers its secret once. The secret is stored only as its digest, and a replay of the issuing
mutation answers without it (§6 of `10-api.md`). Creating the token needs no action of its own:
it is part of issuing, which is governed by the parent's delegation policy alone (§3.2), and a
token that holds one attenuated grant can do nothing its issuer could not have delegated anyway.

A grant also carries a **label**, free text of at most 200 characters, shown wherever the grant is
listed and recorded in the effect that issued it. No authorization decision reads it.

**Why not a link-form grant (§6.1).** A link is spent by use: every call it authorizes takes one of
a use cap it cannot go without, and it can never delegate. A pipeline makes hundreds of calls and
may need to pass narrower authority on, so a link is the wrong shape for it. An API token is
authenticated online on every request, so revoking its grant takes effect at the next request,
which is what a leaked CI secret needs.

> Corrected 2026-10-14 (Finding 45 of `23-walkthroughs.md`). The reason given here was that a
> link is a capability token verified offline, so its revocation latency is its lifetime. A link
> is accepted by the gateway alone, online (§6.1), so revoking one takes effect at its next call
> as a token's does; the difference that remains is the one above.

**The parent may be left unnamed.** When the issuer names no parent, the control plane, which holds
every grant the issuer holds, checks the attenuation against each of them; if exactly one is valid
it becomes the parent, and if several are, the request is refused rather than resolved to one,
because the parent decides what revoking cascades to and a guess would make that invisible. A
caller holding one grant, the common case, never has to look its identifier up.

**What it costs.** A token is a bearer secret with no second factor, exactly as weak as the Phase 0
root token (§9.1), and it is bounded by its grant rather than protected by anything about its
holder; `source_cidr` is the condition that narrows where it works. Online authentication is a
store read on every request that a capability token would not need. A lost secret cannot be shown
again: the issuer revokes the grant and issues another, and the dead token remains as a principal
holding nothing.

---

## 4. Templates: roles in the interface, not in the model

```
GrantTemplate
  name           "Moderator" · "Billing integration" · "Read-only auditor" · "Support, 4 hours"
  version        int             incremented on every edit
  actions        []Action
  scope_pattern  ScopePattern
  conditions     Conditions      defaults, overridable at issue
  delegation     DelegationPolicy   required: a template must say whether its grants may delegate
```

Applying a template **issues ordinary grants** and then plays no further part in authorization.
Three properties follow, and the second is the one RBAC cannot offer:

1. Issued grants are ordinary grants: they attenuate, expire, revoke, and audit identically.
2. **No authorization decision reads the template.** Editing a template later does not silently
   widen authority already handed out. In every role-based system, editing a role changes what
   everyone holding it can do, retroactively and invisibly, a reliable source of production
   accidents. A grant records the template and version it came from as provenance, which tooling
   uses to find it and authorization ignores (§3.5).
3. Authorization code never sees a template. There is exactly one evaluation path (P6).

> Second external review, finding S34. "No back-reference" made the most common operator request,
> changing what every Moderator can do, impossible even to locate. Provenance makes it locatable
> without making it automatic, so the change is a reviewed `Operation` rather than an edit with
> invisible reach.

---

## 5. External identity

```
Subject = User | Token | ExternalIdentity | Workload | Extension
```

```
IdentityBinding
  provider            ProviderRef      the identity provider extension that verifies it (§5.3)
  selector            guild+role · guild+user · oidc issuer+claim · saml issuer+attribute
  subject             ExternalIdentity
  kind                individual | set
  created_by          operator | first_use      sets are always operator-declared
  template            TemplateRef?     for set bindings: what the set's members receive
  constraints         BindingConstraints
  reverify_interval   Duration
  state               verified | unverified
  last_verified_at    Timestamp
```

This is the one definition of the binding; §3.2 of `01-model.md` lists the same fields.

An `ExternalIdentity` is a first-class subject, so a grant can name *the `@moderator` role in
Discord guild 456* directly. Nobody needs a Korpis account. Losing the role removes the authority
with no deprovisioning step, because there was no provisioning step, once something notices (§5.1).

**Set bindings and individual bindings are created differently, and both are visible.** A set
binding (a role, a group) is declared by an operator, because it decides who receives authority. An
individual binding (one Discord user, one OIDC subject) is created on that person's first
authenticated use, because it decides nothing; it only gives a stable subject identifier to
someone whose authority comes from a set or from a grant issued to them later.

### 5.1 Losing a role removes authority only if something notices

> Finding 19 of `23-walkthroughs.md`.

"No deprovisioning step" is a feature and it is also the reason there is no event. Discord does not
tell Korpis that a role membership ended; the role simply stops appearing in the next signed
interaction, and if the person never interacts again, Korpis observes nothing at all. §3.5 makes
revocation cascade to an entire subtree, and that machinery runs on a revocation. Nothing was
revoked here.

So the promise holds for exactly one of the three things a role holder can leave behind:

| What they left | What removal of the role does today |
|---|---|
| a console session | dies within one token lifetime, 120s. **Correct** |
| a child grant they issued to a helper, 7 days | keeps working. **Wrong** |
| a share link, 24 hours | keeps working. **Wrong** |

An external binding is therefore **re-verified on a declared interval** (`reverify_interval`, §5),
not only when its holder happens to interact.

A binding that fails re-verification enters `unverified`, which **suspends every grant rooted in it
and cascades exactly as revocation does**. Suspension rather than revocation, because an identity
provider being unreachable must not silently destroy a delegation tree: `unverified` is visible,
dated, and reversible, and it fails closed for authority while failing loudly for the operator.

**"Fails" has two meanings, and they are treated differently.**

> Second external review, finding S22. Recorded in §17 of `23-walkthroughs.md`.

| The provider | The binding |
|---|---|
| answers, and the membership has ended | `unverified` immediately |
| cannot be reached | stays `verified` until `last_verified_at + reverify_interval + grace`, then `unverified` |

`grace` is declared per provider (default fifteen minutes) and reported beside the interval, so the
longest an unreachable provider can extend a membership is a stated number rather than "until it
comes back". `12-surface-discord.md` §9 had said role-derived grants stay valid until their own
expiry while Discord is unreachable, which is the opposite of this section's fail-closed rule; both
now say this.

**A set binding verifies the set. A member's delegation needs the member verified too.**

> Second external review, finding S23.

Re-verifying the `@moderator` binding confirms the role still exists and still maps where it did.
It says nothing about whether moderator X, who minted a seven-day helper grant last week, is still
in it. Nothing in the chain recorded X: `issued_by` named X as a fact, not as a condition.

So a grant issued **by a member exercising a set grant** records `via: {set, member}` in its chain
link, and its validity requires both the set binding and that member's membership to be verified.
The identity provider re-verifies the membership of every member who has issued a still-valid
grant, on the same interval, which is a bounded set (members who delegated, not members who exist),
and costs one provider lookup per such member per interval. A console session held by a member
needs no extra check, because its 120-second token is renewed through the gateway, which asks the
same question.

**A structural ceiling, so that re-verification is not the only defence.**

> External review, finding R1, which reached the same defect from the algebra rather than from a
> trace and proposed one mechanism this section did not have.

Re-verification bounds how long a stale membership can authorize, and it depends on an interval
someone configures. A second bound that no configuration can widen is cheap here: **a grant whose
chain contains a set-subject anywhere carries a hard expiry ceiling**, independent of what its
issuer asked for. A moderator with a ninety-day role grant can mint a child that outlives their
membership by at most the ceiling, not by ninety days.

The ceiling is seven days by default. It is owned here, set per installation by the root
organization, and can be lowered by any organization for its own subtree; raising it is a root
decision recorded as an effect, because it widens how long authority can outlive its source.

The two mechanisms fail differently, which is why both are worth having. Re-verification catches a
membership that ended while the delegation tree stays intact. The ceiling catches the case where
re-verification itself is misconfigured, unreachable, or set too long, and it does so without
asking anyone to have got a number right.

`reverify_interval` is the honest analogue of token lifetime. It is the bound on how long authority
outlives its source, it is stated rather than convenient, and it applies to OIDC and SAML equally,
because a group claim in a token that was issued an hour ago is exactly the same problem.

### 5.2 The Discord trust boundary, stated plainly

When a Discord interaction arrives, it is signed by Discord (Ed25519) and its payload includes the
member's roles in that guild. The Discord identity provider (§5.3) verifies the signature and the
role list is taken as fact.

**How an interaction becomes authority, without any extension holding it.**

> Second external review, finding S21. Recorded in §17 of `23-walkthroughs.md`.

`12-surface-discord.md` said the bot "exchanges that user's Discord identity for a capability token
carrying that user's grants", while §7 of `16-extensions.md` forbids an extension to receive another
subject's token, and nothing specified the exchange: who verifies the signature, whether a stored
interaction can be replayed, or whether the token is narrowed to the command. Had the exchange gone
ahead as written, the bot would have held every invoking user's full authority. So:

1. **Interactions arrive only by HTTP**, signed. Interactions delivered over Discord's gateway
   connection carry no signature and are not accepted as identity; a bot in gateway mode can read
   and notify, and cannot act for anyone.
2. **The identity provider verifies and asserts.** It checks the Ed25519 signature, rejects a
   timestamp more than five minutes from its clock, and rejects an interaction identifier it has
   already seen within Discord's fifteen-minute interaction lifetime, so a captured interaction
   cannot be replayed. It produces a signed `Assertion {provider, user, sets, interaction, at}`.
3. **The chat client never receives the user's authority.** It calls `InvokeAs(assertion, action,
   resource, arguments)` for the one action the command names. The gateway evaluates the asserted
   subject's grants for exactly that action on exactly that resource and performs it under them.
   The assertion is single-use, so a second action needs a second interaction.
4. **The effect names everyone**: the asserted subject, the grant that permitted it, the identity
   provider that vouched, and the client that relayed it.

A chat client therefore holds its own narrow grants (reading events for the channels bound to it,
§7 of `12-surface-discord.md`) and nothing else. It is the user's conduit for one action at a time,
which is what "holds no authority of its own" has to mean to be true. The same contract serves any
chat platform whose interactions are signed; a platform whose are not can notify and cannot
govern.

**That means Korpis trusts Discord's assertion of who holds which role.** Anyone who obtains
*Manage Roles* in that guild can grant themselves whatever Korpis granted that role. This is not a
flaw to be engineered away; it is the necessary consequence of accepting an external identity
provider, and it is true of OIDC and SAML equally. What matters is that it is stated rather than
discovered.

Two consequences are built in:

**Default: grants to set subjects and link-form grants cannot include irreversible actions.** The
irreversible actions are the ones the action vocabulary marks so (§5 of `10-api.md`): deleting a
volume, rolling a volume back to a snapshot, deleting a snapshot, deleting a workload, releasing an
address, and pruning backups. They are excluded unless an
operator deliberately overrides the default for that binding, having read what it means. The rule
is enforced at attenuation, by the gateway; a chat client may hide what a user cannot do, as a
courtesy, and never decides it (finding S35).

**`requires_mfa` cannot be satisfied by a Discord identity alone.** A grant demanding MFA needs a
Korpis-native authentication factor. Discord's own 2FA state is not visible to Korpis and would not
be trustworthy if it were.

The result is a usable split: routine operations (start, stop, restart, read console, view files)
flow from Discord roles with no friction, which is the whole point of Bet 1. Destructive operations
require a stronger identity or an approval from one (`requires_approval_by`), which is what makes
granting a chat role real authority defensible.

### 5.3 OIDC and SAML

Standard, and the same shape: an issuer plus a claim maps to an `ExternalIdentity`, grants are
issued to it, group claims work exactly like chat roles including the same trust boundary. SSO is a
paid-tier feature in the commercial products and absent from the open ones. Here the machinery is
core (external identities, bindings, templates, assertions, `InvokeAs`), because it is the same
mechanism an external chat identity already requires.

**The adapters are extensions, and an identity provider is in the trust base by definition.** OIDC,
SAML, LDAP, and Discord are spoken by identity provider extensions (§2 of `16-extensions.md`). A
provider decides who is who, so whatever it asserts is believed, exactly as a certificate authority
is. That is not a weakness of running it out of process; it is what an identity provider is.
Installing one into an organization therefore requires authority over that whole organization
(§4.1 of `16-extensions.md`), its assertions are signed with a key registered at installation, and
it can assert identities and memberships and do nothing else: it holds no grants that let it act.
`12-surface-discord.md` §10 called this machinery core and `16-extensions.md` §2 called the
providers extensions; both are right about different halves, and this paragraph is the seam.

---

## 6. Capability tokens: the wire form of a grant

> Resolves open question 2 of `02-architecture.md`, and the emergency-console half of question 1.

The insight that makes this coherent: **a grant and a capability token are the same object in two
representations.** A grant is the stored form; a token is the signed, self-contained wire form.
Identical attenuation algebra, identical evaluation.

```
CapabilityToken
  grant_chain     the identifiers of the attenuation chain, leaf first
  actions, scope  one concrete object; selector scopes are resolved before minting
  issued_at, expires_at
  epoch           the issuing control plane's key epoch
  source_cidr     the leaf's networks, if it has any, for the relay to check the client against
  stream_range    the leaf's range of a stream, if it has one, for the node reading it to hold
                  the session to (§8 of `14-streams.md`)
```

**The token is a Biscuit** whose authority block is signed by the control plane's token key and
states the chain, the actions, the scope, and the expiry. The gateway mints one only after it has
evaluated the whole chain online, every link's attenuation and delegation policy included (§3.2),
so a token is the *result* of an authorization rather than a claim a verifier has to re-derive. A
holder can attenuate a token further, offline, by appending a block, and a block can only add
checks (a shorter expiry, a narrower operation), never authority. Implemented in
`crates/korpis-core/src/authz/token.rs`.

> Corrected 2026-10-12, while implementing it. This said each link of the chain was signed, and
> the table of §6.2 had offline verifiers checking every link's delegation policy. Neither is
> needed and neither was what made the design safe: the delegation policy constrains who may
> *create* a grant, which happens online at the gateway, and an offline verifier has nothing to
> create. What a verifier needs from the chain is its identifiers, so that the revocation list can
> refuse a token when any link is revoked (§6.3), and that is what the token carries.

Two conditions travel with a token because their verifier is offline. The leaf's `source_cidr` is
carried because the relay is the first hop that sees the client's address (§3.1 of
`02-architecture.md`) and must check it there; the leaf's networks are enough, because attenuation
keeps each child's networks within its parent's (§3.2). An agent behind a relay never sees the
client and does not check it. And a private action (§3.3) is judged offline against the resource's
ancestry, which a relay does not hold: so a token whose scope is the resource itself verifies
without any, because the gateway judged that very object when it minted it, and a token scoped
above the resource is refused by a verifier that cannot see what lies between. The gateway always
mints for the one object the request names, so the first case is the only one a client meets.

The relays and the agents hold the control plane's public key and verify a token **offline**. This
is what makes interactive work possible without a round trip per keystroke:

```
client attaches to console
  → gateway evaluates the grant chain (online, authoritative)
  → issues a capability token: {workload.console.write on workload W, expires in 120s},
    and the route: the node W is placed on, sealed so the client cannot read it
  → the client presents both to a relay, which verifies the token offline and opens the
    route to learn the node
  → the agent verifies it again, offline, before attaching
  → keystrokes flow client → relay → agent, never through the control plane
  → renewal every 120s requires the gateway
```

> Second external review, finding S19. The first version of this flow ended with "keystrokes flow
> directly", to an agent that by §3 of `02-architecture.md` has no inbound port. The relay of §3.1
> of `02-architecture.md` is what the word "directly" was standing in for.

**Revocation latency equals the token lifetime.** That is the honest trade, and it is why the
lifetime is short and configurable rather than convenient. Revoking a grant stops the *next*
renewal, not the current window.

> Corrected 2026-10-14, from the implementation. That is the bound when the control plane is
> unreachable, and it holds. While it is reachable the revocation list (§6.3) does better, and
> this paragraph predated it: a node ends every live session whose chain a new list names, with
> the reason `revoked`, within about two seconds of the revocation, without waiting for the
> renewal. A session renews in band: the client sends a fresh token on the session before the
> one it holds expires, the relay and the node verify it as they verified the first, and it must
> be the same subject's; a session whose token expires unrenewed is ended by the node as
> `expired`, and one whose renewal the relay refuses as `refused` (§3.1 of
> `02-architecture.md`).

Two things fall out:

**Console survives brief control plane outages**, for exactly one token lifetime, then it stops.
Bounded, predictable, and stated.

**Break-glass access is the same mechanism.** An operator can pre-issue a long-lived token, held
offline, that agents accept when the control plane is unreachable (open question 1 of
`02-architecture.md`). It is a real capability with real risk, so: its use is recorded locally and
replayed to the effect log on reconnect, it is revocable by rotating the control plane key epoch,
and its existence is visible in the interface rather than being a secret an operator forgets they
created.

**A restored control plane rotates the key.**

> Finding 9 of `23-walkthroughs.md`.

Offline verification means a token's validity does not depend on the database that recorded it.
That is the property this whole section is built on, and it has a failure mode: restore an older
backup and a grant that was issued *and revoked* after it is gone from the database while its
token, signed by a key restored alongside, still verifies. Authority in circulation, no row
recording it, and an audit trail that disagrees with what is actually happening.

So key rotation is a mandatory step of control-plane restore, alongside the lease epoch fence (§8
of `03-state.md`). Every token issued after the backup stops verifying and every session
re-authenticates, because a control plane that cannot account for a grant must not honour it.

This is also why token lifetimes being short is a structural decision rather than a tuning knob:
the same number that bounds revocation latency bounds how long a restore's disruption lasts.

**Two keys, and restore rotates only one.**

> Finding 16 of `23-walkthroughs.md`.

"Rotate the signing key" is ambiguous while there are two keys in this design with two different
jobs, and the ambiguity decides whether disaster recovery ends with a working fleet:

| Key | Signs | Rotated on restore |
|---|---|---|
| the **grant signing key** | capability tokens, verified offline by agents | **yes**, mandatorily |
| the **node identity certificate** | the per-node key pinned at enrollment (§3 of `18-operations.md`) | **no** |

They are separate because node identity and delegated authority have different lifetimes and
different revocation stories, which is the same reasoning that separated every other authority in
this design. Rotating both would end a recovery with an operator re-enrolling every node by hand,
during the incident, which converts the outage the restore was fixing into a longer one.

### 6.1 The shareable link

Bet 1's sharpest edge, and the operation RBAC cannot express:

```
https://korpis.example/g/#<token>
```

The fragment **is** the capability token. Holding the URL is holding the authority.

```
Moderator holds: workload.restart on project Survival, expires in 90 days
        attenuates to
Link:            workload.restart on workload W, expires in 24h, max_uses 3
        sent in a Discord DM, no account, no invitation, no provisioning
```

It cannot outlive the moderator's own grant. Revoking theirs kills it. Every use is recorded in the
effect log with the full chain, so "who restarted it" resolves to *this link, attenuated from this
moderator, issued at this time*.

**It is a bearer token, and that is said out loud.** Anyone holding the URL holds the authority.
Mitigations, all of them partial:

- The token is in the **fragment**, never the path or query, fragments are not sent to servers and
  do not appear in access logs or `Referer` headers.
- Short expiry and use caps are defaults, not options, and the share screen shows both before the
  link exists (§6 of `11-surface-web.md`). A use cap is redeemed online (§6.2), which is a real cost
  and is stated where the link is created rather than discovered during an outage.
- High-risk actions are excluded from link-form grants by the same default that governs chat roles
  (§5.2), and a link-form grant can never delegate (§3.2).
- **A bearer link is never placed in a message more than one person can read.** A surface that
  wants to offer "open this in the panel" from a shared place (a channel, a ticket) offers a button
  whose press mints a fresh link for the presser, under the presser's own authority (§6 of
  `12-surface-discord.md`). A token in a public message is a grant to everyone who scrolls back.

**What counts as a use.** A use is a **redemption**: presenting the link to obtain a session or
perform an action. The short-lived tokens that keep an established session alive are renewals of
that redemption, not new uses, so a console opened once through a `max_uses: 3` link consumes one
use however long the session lasts, up to the link's own expiry. Without that rule a three-use link
would have lasted six minutes of console.

**Not bound to the IP of first use.**

> External review, finding R14.

An earlier draft offered this as a hardening option. It does not survive contact with the people
the feature exists for: a moderator opening a link on a phone is behind CGNAT or moving between
cell towers, and their address changes minutes after first use. An option that breaks the intended
user in ordinary conditions is not a security control, it is a support ticket that also teaches
people to turn security controls off.

Online redemption (§6.2) is the mechanism that actually bounds a leaked link, and it bounds it by a
number the issuer chose rather than by a network property nobody controls. `source_cidr` remains
available for the case it was designed for, a link issued to a known fixed network.

**A link is accepted by the gateway alone.**

> Corrected 2026-10-14, when links were built (Finding 45 of `23-walkthroughs.md`). §3.7 said a
> link is a capability token verified offline, so that its revocation latency is its lifetime.
> Asked what would verify it offline, the answer was nothing that could use it. Every use of a
> link begins at the gateway: an action is an API call, and a session needs the route that only
> the gateway seals (§3.1 of `02-architecture.md`). And a relay or agent that accepted a link
> would accept a revoked one: the revocation list keeps a grant for one session-token lifetime
> (§6.3), so a 24-hour link, accepted offline, outlives its own revocation by a day.

So a link is a Biscuit signed by the token key, like every token, that states only the grant it is
the bearer form of and its expiry. It states no right and no scope, and the offline check that
relays and agents run allows only a right a token states on a scope it states, so they refuse a
link for every action. The gateway accepts it as a credential, online: the signature against the
current token key, the expiry, and then the grant as it stands, through the same evaluation as
any grant, so expiry, revocation, and the use it spends are checked where they are true.
Revoking a link takes effect at its next call; a session it opened ends within about two seconds,
because its tokens' chains name the link's grant and the revocation list does.

**As built, 2026-10-14.** `korpis grant share --actions workload.restart --scope workload:W` issues
a link-form grant and prints it once as `https://<control plane>/g/#korpislk_…` (§7 of
`13-surface-cli.md`); the holder of the grant is the link itself, a subject `link:<grant>` that
no one can be named as. The issuer must give it an expiry and a use cap, which the command
defaults to 24 hours and three uses and the server refuses to go without, because "defaults,
not options" means a link never exists without them; and it is for one object, never a selector,
because its recipient is shown exactly what it reaches. A replay of the issuing mutation answers
without the link, as it does for a token's secret. Its holder can read the link's own grant, which
is how a client shows what it does, including who issued it, whom the recipient knows already;
not the chain above it, which is the issuer's own authority up to the organization's root.

A use is spent wherever a grant is used. The gateway spends one when it authorizes a call; minting
a session token spends one for the token, whatever it carries; and minting its successor while it
lives spends none, because the request names the token it renews, and the gateway checks it is
the same holder's, through the same chain, on the same object, with no action it did not carry.
That is how a three-use link opens three consoles of any length. Building it found that nothing
else spent a use either way (Finding 46 of `23-walkthroughs.md`): minting refused a use-limited
grant outright, so a link could not open a console at all, while list filtering, event delivery,
and plan approval accepted one without spending it, so a link could be used without limit
through them. Now a check that spends nothing permits nothing to a use-limited grant, and
approval spends a use. A workload's watch spends one when it opens and none for the reads that
keep it current, which continue through the grant that paid, as a renewal does, until it is
revoked or expires (Finding 76 of `23-walkthroughs.md`). Likewise the proposer confirming or
rejecting its own plan, through the grant it was proposed under, continues the call that
proposed it and spends nothing, so an action proposed and then confirmed, as the panel does
every action (§4 of `11-surface-web.md`), is one use.

What it costs: a link is unusable while the control plane is unreachable, which every link with a
use cap already was (§6.2), and which every link now is. Rotating the token key ends every link
with every token, so a control-plane restore (§6) ends links issued before the backup as well as
after. A holder can append a block that shortens a link before passing it on, and nothing else:
the gateway believes only the block the control plane signed for what the link names.

---

### 6.2 `max_uses` is redeemed online, and says so

> External review, finding R2. Recorded in §16 of `23-walkthroughs.md`.

An agent verifying a token offline can check two of a grant's three limiting conditions. A
signature is self-contained. An expiry is a number compared against a clock. **A use counter is
neither**: it is shared mutable state, and counting it needs either a central point of redemption
or coordination between every agent that might see the token. This design specified neither, which
made `max_uses` a limit the interface displayed and nothing enforced, in the document that tells
every other document not to do that (K-3, P4).

So the rule is stated rather than assumed:

| Condition | Checked where | Survives a control plane outage |
|---|---|---|
| signature, and the chain's identifiers against the revocation list (below) | relay and agent, offline | **yes** |
| `expires_at`, `not_before` | relay and agent, offline | **yes** |
| revocation of any link | relay and agent, against the last revocation list, offline | **yes**, as of the last list received |
| `source_cidr` | the gateway for API calls; the relay for sessions; offline at the relay | **yes** |
| `requires_mfa`, `requires_approval_by` | gateway, online | no |
| **`max_uses`** | **gateway, online, redeemed atomically** | **no** |

A token carrying `max_uses` is redeemed at the gateway, which decrements the counter in the same
transaction that authorizes the call. Every share link carries one (§6.1), and is accepted by the
gateway alone. It is therefore **not** usable while the control plane is
unreachable, and a token without `max_uses` is. That is the trade and it is the right way round:
the feature that needs a counter is the one that gives up offline operation, rather than every
token giving up a guarantee to support one condition.

**A use is spent when the call is authorized, not when it succeeds.** The gateway takes the use by
compare-and-swap on the grant row the moment the grant permits the call, and the call proceeds
only if the swap won. A call that then fails, a duplicate name or a conflict, has still spent its
use. The alternative, spending the use in the transaction of the change itself, would make every
use case responsible for remembering to do it, and one that forgot would be a use limit displayed
and not enforced. So the counter can be spent by a failed call, it can never be exceeded, and a use
case that must not waste a use (enrollment, §3 of `18-operations.md`) checks what it can before it
asks.

> Corrected 2026-10-13, from the implementation. This said the counter is decremented "in the
> same transaction that authorizes the call", which the implementation read as the transaction
> of the change, and then redeemed nothing: no use case held the grant's limit, so `max_uses` was
> stored and never counted. It is now counted at authorization, for every use case at once.

The cost is stated where a user meets it. A share link with a use limit is described in the
interface as requiring the control plane; the console token of §6, which carries no use limit,
keeps its documented survival window. Break-glass tokens (§6, below) never carry `max_uses` at all,
because a credential whose entire purpose is working during an outage cannot depend on a counter
that does not.

**Issuing is also a race, and it is the same race quota had.**

> External review, finding R3.

`child.max_uses ≤ parent.remaining_uses` (§3.2) reads `remaining_uses`, which is the one mutable
field on an otherwise immutable object. Two concurrent attenuations of a parent with three uses
left each observe three and each mint a child with three. This is Finding 1 of `23-walkthroughs.md`
exactly, on the authority side instead of the capacity side, and it takes the same answer:
**`remaining_uses` participates in a compare-and-swap on the parent grant row, in the transaction
that inserts the child.** A losing writer gets `CONFLICT` and re-attenuates against the new figure.

### 6.3 Revoking one thing without revoking everything

> External review, finding R13.

§6 says a break-glass token is "revocable by rotating the control plane key epoch". That is true
and it is a sledgehammer: rotating the key invalidates every token in the system, so killing one
pre-issued credential logs out every console session in the fleet.

Break-glass tokens are few and long-lived, which is exactly the shape a revocation list handles
well. Relays and agents hold a **signed revocation list of grant identifiers**, distributed on the
same channel as the public key (`RevocationList`, §4.2 of `02-architecture.md`) and cached with the
same rules, and **a token is refused if any link of its chain is on the list**. It is checked
offline, so it costs nothing that matters.

> Second external review, finding S30. Recorded in §17 of `23-walkthroughs.md`. The list held token
> identifiers. With revocation derived from the chain (§3.5), revoking the *parent* of a break-glass
> token left the token verifying offline unless the control plane walked the subtree to find every
> descendant token, which is the walk R4 had removed. Listing grant identifiers and checking every
> link makes the cascade work offline with no walk.

An entry stays on the list until every token whose chain could contain it has expired, which is
bounded by the longest-lived outstanding token, and only grants revoked within that horizon are
listed. Short-lived tokens never need an entry of their own; their chains are covered by their
ancestors'.

**As built.** The list is signed by the token key, not the assignment key, because the parties
that read it are exactly the parties that verify tokens, and a relay holds the token key and has no
use for the other. The signature covers a canonical form bound to its purpose
(`korpis.revocation.v1`, the version, then each identifier in order), so nothing else the token key
signs can be read as a list. An entry is a grant revoked, or suspended and not lifted, within the
horizon, and the horizon is the longest lifetime of any token this control plane mints (120 s,
the session token of §6) plus the clock skew an agent session allows (§4.2 of
`02-architecture.md`), because a verifier whose clock runs behind still honours a token the
control plane already considers expired. A longer-lived kind of token, break-glass among them,
raises that bound in the same change that introduces it, or it outlives its own revocation. A
share link does not raise it, because no verifier that reads the list accepts a link (§6.1).

Every replica computes the list from the same rows every two seconds, and publishes it to the
sessions it holds when the identifiers changed; a session sends the current list when it opens and
every list published after. No replica tells another anything, so a revocation reaches a connected
verifier within about two seconds of its commit, through whichever replica that verifier is
connected to. Each list replaces the last whole, so a lifted suspension leaves the list on the next
computation. The agent believes a list only once its signature verifies against the token keys
enrollment returned, keeps it in its state directory with that signature (`revoked.binpb`), and
verifies the kept copy again when it starts, so a restarted agent refuses the same chains before
it reaches the control plane. It takes a list with a lower version than the one it holds: replicas
compute independently, and refusing a lagging replica's list would keep one missing a later
revocation no longer than the next computation, while accepting it loses at most the two seconds
it lags. What it does not do yet: a suspended organization (§2.1) and an unverified binding (§5.1)
suspend grants by derivation, and the list carries only grants whose own row says so, so their
tokens are refused at renewal rather than within seconds (question 7, §10).

Key rotation remains the answer to *"the key may be compromised"* and to control-plane restore
(§6). It stops being the answer to *"this one credential should stop working"*.

---

## 7. What a provider can see

Bet 4 makes hosting providers parents of their customers' organizations, which raises a question
every panel avoids: **can my host read my files?**

The physical answer is always yes, they own the hardware. The design question is whether the
platform makes that invisible.

| Data | Parent organization's default access |
|---|---|
| Resource usage, quota consumption, metering | **yes**: they are paying for it |
| Workload existence, names, state, health | **yes**: they operate the capacity |
| Placement, node assignment | **yes** |
| Console content | **no**, requires an explicit grant |
| File contents | **no**, requires an explicit grant |
| Backup manifest, the file tree without contents | **no**: requires `backup.browse`, and browse does not imply read (Finding 13 of `23-walkthroughs.md`) |
| Backup contents | **no**, requires an explicit grant naming the tenant (§3.3), and under `tenant` key custody not even then (§5.5 of `06-storage.md`) |
| Environment variables and secrets | **no**, requires an explicit grant |

Operator access to tenant data is itself a grant. It can be issued (support cases require it) and
when it is used, **the effect is visible to the tenant whose data was accessed**, not only to the
operator's audit log.

This is a stance no competitor takes, and it costs nothing to implement given the model. It
converts "your host can read everything and you will never know" into "your host can read this, and
you will see when they do."

---

## 8. Explanations are filtered by grant

> Resolves open question 5 of `05-scheduling.md`.

The `Explanation` attached to a `Placement` (§2.3 of `05-scheduling.md`) contains node identities,
other nodes' capacity, and why candidates were rejected. Showing that to a customer leaks the
provider's infrastructure and, through utilization figures, information about other tenants.

**Explanations are stored complete and rendered per viewer.** The stored object is the full
derivation. What a subject sees is filtered to what their grants cover:

| Viewer | Sees |
|---|---|
| Operator (root org) | everything, every candidate, every score, every node |
| Tenant | "placed in region EU on a node offering the `microvm` tier; 3 candidates considered" |
| Workload-scoped grant | placement region and tier only |

Same rule applies wherever an object aggregates across a tenancy boundary, node status, cluster
capacity, scheduler recommendations. Filter on read; never store two versions of the truth.

The table rests on grants the gateway already evaluates, one load of the viewer's grants for every
node an explanation names:

| What | Shown to |
|---|---|
| a node's rejections and scores, and whether it was chosen | a viewer with `node.read` on that node |
| how many candidates, and how many nodes failed each check | a viewer with `workload.read` over the workload's organization: the tenant |
| which checks failed, with no count | anyone who may read the workload |
| the region (the chosen node's `region` label) and the tier | anyone who may read the workload |
| the policy | a viewer who may read every node the explanation names |

A placement's `reason` is written for every reader, so it names no node and no count. The same
filter applies to the explanation of an unplaced workload (§2.3 of `05-scheduling.md`), which is
the explanation a customer asking "why is my server not starting" is shown: the checks that
failed, never whose nodes failed them.

---

## 9. Authentication

| Factor | Notes |
|---|---|
| Passkeys / WebAuthn | **the default and the recommendation**, phishing-resistant, and the reason it is first here |
| TOTP | supported, second class |
| Password | supported, always with a second factor available |
| OIDC / SAML | §5.3 |
| Discord | §5.2, with its stated limits |
| Node enrolment token | single-use, short-lived, itself an attenuated grant (§7 of `02-architecture.md`) |

`requires_mfa` on a grant is satisfied by a passkey or TOTP, never by an external provider's
assertion.

> Second external review, finding S33. Recorded in §17 of `23-walkthroughs.md`. The four
> subsections below were missing: the design said how authority flows and not how the first
> authority comes to exist, how a person who loses a factor gets back in, or how long a login lasts.

### 9.1 The first grant

`korpis-server install` creates the root organization and prints a **bootstrap token**: single-use,
valid for one hour, redeemable only by registering a passkey. Redeeming it creates the first user
and issues the root grant to that user, with unbounded delegation (§3.2). The token is then void.
Every other grant in the installation descends from that one.

> Phase 0 note, 2026-10-12. Registering a passkey needs the web client, which arrives in Phase 1
> (§4 of `20-roadmap.md`). Until then `install` issues the root grant to an **API token** whose
> secret it prints once, and the effects of installation name the `System` bootstrap actor
> (§3.2 of `01-model.md`). This is a weaker root than a passkey, and it is stated rather than
> hidden: the token is a bearer secret with no second factor. When the web client exists, the
> bootstrap token above replaces this path, and an installation's Phase 0 root token can be
> revoked once a passkey-held root grant has been issued through local root recovery (§9.2).

> Designed 2026-10-16, for the web client of Phase 1. The bootstrap token is an **invitation**
> (§9.6): `korpis-server install` creates the first account, holding the root grant and no factor,
> and prints its invitation, redeemable within the hour and only by registering a passkey, in the
> panel at the installation's own address. (Corrected the same day, with §9.6: this said redeeming
> the invitation created the user and issued it the root grant.) The Phase 0 path stays for what
> cannot register a passkey, an unattended install or a test: `korpis-server install --root-token`
> issues the root grant to an API token as before, and says when it does that this root has no
> second factor. One installation uses one path or the other, never both, so a scripted install is
> not a passkey-held root with a bearer secret beside it.

> As built, 2026-10-16. `korpis-server install` creates the account and its root grant and prints
> the invitation, its expiry, and that only registering a passkey redeems it; with `--root-token`
> it prints the token and says it has no second factor. Until the panel of `11-surface-web.md`
> is built, later in Phase 1, redeeming the invitation needs a client of the account service of
> its own, so an installation that must be usable before then is installed with `--root-token`.

### 9.2 When the last administrator is locked out

Whoever holds root on the machine running the control plane already holds everything: the
database, the signing key, the process. Pretending otherwise would be the kind of claim §8 of
`17-security.md` refuses. So recovery from a lost root grant is honest about where it comes from:
`korpis-server admin recover-root`, run locally by the machine's root user, issues a new root grant
to a newly registered passkey and writes an effect saying exactly that, which every tenant whose
data is in scope can see. It needs no network listener and no secret question, and it cannot be
done from anywhere except the machine.

> Designed 2026-10-16. A passkey is registered in a browser, and the machine's root user may have
> none, so `recover-root` does not register one itself: it creates, straight in the database, a new
> account holding a new root grant, and prints the account's invitation, which only registering a
> passkey redeems, in the panel, as at installation. The effect it writes names the `System`
> bootstrap actor and the operating-system user who ran it.

> As built, 2026-10-16: `korpis-server admin recover-root --data-dir --database-url` refuses a
> directory that holds no installation's server certificate, and the effect `root.recovered`,
> recorded under the new root grant, names the account, the grant, and the operator as the
> process's user id with the `USER` and `SUDO_USER` it was run with. The user id is the kernel's;
> the two names are what the shell said, which whoever runs it as root controls, so they say who
> claimed to run it and the user id says with what authority.

### 9.3 Account recovery

- **Several factors per account are the norm**, not the exception, and the interface asks for a
  second passkey at enrolment.
- **Recovery codes** are generated at enrolment, shown once, and each works once.
- **There is no email-only reset by default.** An emailed link is a phishable bearer credential that
  bypasses every factor the account was protected with. An operator may enable it for their
  installation, and the setting says what it costs.
- **An organization administrator can recover a member** in the only way the model allows: by
  issuing grants to the member's newly registered identity. Nothing is "reset"; the old identity's
  grants are revoked or left to expire, and both actions are effects.

### 9.4 Sessions

| Credential | Lifetime | Held by |
|---|---|---|
| web session | refresh credential in an `HttpOnly`, `SameSite=Strict` cookie, rotated on every use; 12 hours idle, 7 days absolute, both configurable | the browser |
| CLI session | a refresh credential from the OAuth device flow, stored in the OS keychain | the user's machine |
| capability token | 120 s, minted per action or session from the refresh credential | memory only |
| workload credential | 15 minutes, renewed from the node that runs the workload (§9.5) | the workload's memory |

Every live session is listed for its user with where and when it was created, and any of them can
be ended, including "end all others", which revokes the refresh credentials immediately and lets the
outstanding capability tokens expire within their lifetime. A session never outlives the grants it
exercises: each token is minted against current authority.

**How a session works, as designed 2026-10-16.** Signing in (§9.6) creates a session and sets
its **refresh credential**, a random 256-bit secret of which the control plane keeps only the
digest, in a cookie that is `HttpOnly`, `Secure`, `SameSite=Strict`, and scoped to the path of
the session service, so the browser sends it nowhere else and no script can read it. The panel
exchanges it for an **access token**: 120 seconds, signed by the control plane's token key, naming
the user and the session. The access token is the bearer of every API call; it is verified by its
signature and expiry alone, and the authority of each call is evaluated then, from the grants the
user holds then, so the token carries no authority of its own and a revoked grant stops working at
the next call. Each exchange rotates the refresh credential, and presenting one already rotated
ends the whole session, since only a copy can present it twice. An idle session ends after 12
hours without an exchange and any session after 7 days (`--session-idle`, `--session-max`).
Ending a session, or all others, deletes their refresh credentials at once; their access tokens
then expire within 120 seconds, the bound this table always gave.

> As built, 2026-10-17. A redemption or a sign-in begins the session and sets its refresh
> credential as the cookie `__Secure-korpis_refresh`, `HttpOnly`, `Secure`, `SameSite=Strict`, and
> `Path=/korpis.v1.SessionService/`, the path of `SessionService`, whose `Exchange` reads it and
> replaces it. The CLI sends its credential in the request body instead and receives the next one
> there. Every digest a session rotated away is kept for the session's life, so a rotated
> credential presented again is recognised whenever it is, and ends the session, recorded as
> `session.reused`; a session ended by age or idleness is forgotten within a minute, with the
> digests it rotated. The access token is verified at the gateway with the control plane's
> current token key, so rotating that key (§8 of `03-state.md`) signs every browser out within
> 120 seconds, as it ends every capability token. The flags are `--session-idle` and
> `--session-max`, written `12h` and `7d`. Exchanging a credential and signing out with it carry no
> idempotency key, as the ceremonies of §9.6 do not (§6 of `10-api.md`). What strict rotation
> costs: an exchange whose answer is lost, retried, presents a rotated credential and ends the
> session, and so would two tabs of one browser exchanging the same cookie at once. So the panel
> exchanges from one tab at a time, holding a lock of the browser's (the Web Locks API) for the
> exchange and sharing the access token it gets with its other tabs, and a client that lost an
> answer signs in again; a grace window in which a just-rotated credential still worked would
> spare that, and would let a copy taken in the same moment work too.

> Corrected 2026-10-17, before the panel used it (Finding 72 of `23-walkthroughs.md`). The cookie
> was `__Secure-korpis_refresh`, scoped to the session service's path, and the design above said
> the browser sends it nowhere else. A path is not a boundary a browser keeps between the pages
> of one site, and a `__Secure-` cookie may be set for a parent domain, so a workload served
> under a sibling name of the installation's, which a host serving its customers' workloads under
> its own domain gives every customer, could set one the control plane could not tell from its
> own (RFC 6265 §8.5 and §8.6, `research/evidence.md` §5), and sign a browser in to a session of
> its choosing. The cookie is now `__Host-korpis_refresh`, which a browser keeps only when it was
> set by the control plane's own name, with no `Domain` and `Path=/`. What that costs: the browser
> sends it with every request to the control plane, not only the session service's; nothing but
> `Exchange` and `SignOut` reads it, and `SameSite=Strict` still keeps it off every request another
> site starts.

> Corrected 2026-10-16. The table's third row said a browser held "capability tokens" minted from
> the refresh credential. What a browser presents to the API is the access token above, which
> names the user rather than carrying a grant chain, because the control plane evaluates the
> user's authority on every call and needs no chain from the caller. Capability tokens (§6) are
> still minted, through the API with an access token, for what is verified offline: a console
> session at a node or a relay.

The CLI's session is the same with a different first step: `korpis login` runs the OAuth device
authorization grant (RFC 8628), the user approves the code in the panel, and the CLI receives a
refresh credential, which it keeps in the operating system's keychain, exchanges for access
tokens, and rotates as the panel does. Where no keychain is reachable, a server without a desktop
session, it says so and keeps the credential in a file readable by its user alone, and
`korpis login --no-store` holds it in memory for one command's life instead.

> As built, 2026-10-17, the control plane's half; the CLI's is the note of the same day in §7 of
> `13-surface-cli.md`. `SessionService.AuthorizeDevice`, called without a credential, answers a
> device code, `korpisdv_` and 256 random bits of which the control plane keeps the digest, and a
> user code of eight characters from the twenty consonants RFC 8628 §6.1 recommends, shown as
> `BCDF-GHJK`, with the panel's page to type it into, `--public-url` followed by `/device`, and
> refuses without a public URL, there being no panel to approve in. It waits ten minutes. The CLI
> polls `DeviceToken` every five seconds and is answered `device.pending` until the code is
> decided; a poll sooner than its interval is refused `RATE_LIMITED` and lengthens the interval by
> five seconds (RFC 8628 §3.5). A signed-in user reads the authorization by its code, shown
> where it was asked from and what the CLI said it was, and approves or denies it; approving needs
> a recent sign-in, as a change of factors does (§9.6), because it begins a session as long-lived
> as a browser's, and both are effects (`device.approved`, `device.denied`). The next poll after
> an approval begins the CLI's session, answering its refresh credential in the body, and spends
> the authorization; one denied is answered `device.denied`, one expired `device.expired`, and
> either is then forgotten, as every expired one is within a minute. What this does not stop: a
> stranger can begin an authorization and talk someone into approving its code, the remote
> phishing of RFC 8628 §5.4, which no protocol can tell from the holder's own CLI. The panel
> says it is approving a sign-in for a command-line client, and where that client asked from,
> which is what the RFC recommends and all it can do; the user who approves anyway has signed the
> stranger in. A user code is guessed only by a signed-in user, inside their own rate limit, and
> must name one of the few authorizations pending at the time, among 20^8.

> Corrected the same day (Findings 74 and 75 of `23-walkthroughs.md`, from a security review of
> the commit that built it). An approval was a fact about the account alone: the poll that
> collected it began the CLI's session whether or not the account could still sign in, and
> whether or not the session that approved it had since been signed out or ended, so a holder who
> found a stolen panel session and ended every other session could not withdraw an approval that
> session had given and its thief had not yet collected. An approval now names the session it
> was given from, and is honoured only while that session lasts and its account may sign in;
> otherwise the poll is answered `device.denied` and nothing is begun. What it costs: a user who
> approves a code and signs out of the panel before the CLI's next poll, five seconds at most,
> has approved nothing.

### 9.5 A running workload's identity

> Added 2026-10-13, while designing the extension contract of Phase 1. `Workload` and
> `Extension` were subjects since the first version (§3.2 of `01-model.md`), and nothing said how
> either proves who it is. An extension is a workload (§3 of `16-extensions.md`), so the answer is
> the same for both, and every extension call depends on it.

**A workload holds no long-lived secret. Its node vouches for it, and only while it runs there.**

The agent serves each workload it runs a Unix socket of its own, whose directory the driver mounts
read-only into the workload at `/run/korpis`, so the socket is `/run/korpis/identity.sock` (§6 of
`02-architecture.md`). A process in the workload asks it for
a credential. The agent forwards the request over its stream, naming the workload the socket
belongs to, and the control plane issues the credential only if the workload is assigned to that
node, authoritatively, under a lease that has not expired. The workload is then authenticated as
itself: `workload:<id>` for an ordinary workload, `extension:<id>` for an extension's (§3.2 of
`01-model.md`), and it holds exactly the grants issued to that subject.

The credential is a bearer secret of 256 bits that expires after fifteen minutes. The control
plane stores only its digest, as it does for an API token (§3.7), together with the node and the
lease epoch it was issued under. The workload renews it by asking the socket again, so a renewal is
always a fresh proof that the node still runs it.

A workload can ask before its node holds the lease: the socket exists from the moment its
container is built, and the lease is obtained over the same stream as the credential, so the two
can cross. The control plane then refuses with `credential.no_lease` marked
retryable, and the socket answers `unavailable`, which tells the workload to ask again, as it does
when the control plane cannot be reached. Only a refusal that will not pass, a workload placed
elsewhere, is `permission_denied`. Added 2026-10-15, from the implementation: the first version
answered every refusal as denied, so a workload asking as it started was told, wrongly, that it
never would be.

**An installed extension reaches the API through its node.** Added 2026-10-15 (Finding 57 of
`23-walkthroughs.md`). The agent serves an installed extension's workload a second socket in the
same directory, `/run/korpis/api.sock`, and relays each connection on it, byte for byte, to the
control plane's API at the address it was enrolled with, over the host's own network. The extension
speaks TLS through the relay to the control plane itself, verifying it under the CA the identity
socket gave it and naming it by the name in the address the socket gave it, so the agent carries
what it cannot read, and the credential it already could obtain is all it could misuse. The control
plane marks which workloads are installations in their assignments, and the agent serves the socket
to those alone. This is what lets an extension run on a single machine, where the node reaches the
control plane on loopback, which no workload's namespace reaches, and on a host whose firewall
closes its input to workloads; and it needs no exception to the egress default (§6 of
`07-networking.md`). What it costs: an extension's traffic to the control plane, a provider's open
streams included, passes through its node's agent, so an agent that stops stops its node's
extensions' calls with it, which a provider's reconnection then recovers (§3.3 of
`16-extensions.md`).

**What evaluates this, where, and with what information.** The control plane, at issue, from its
own placement and lease records and from the identity of the node session the request arrived on
(§7 of `02-architecture.md`). Nothing on the node decides that a workload is who it says: the agent
only says which socket a request came in on, and the socket is the workload's because the agent
created it for that workload's container alone.

**What it does not stop.** A compromised node can obtain credentials for every workload assigned
to it, which is no more than it already has, since it runs them and reads their memory (§9 of
`17-security.md`). It cannot obtain one for a workload placed elsewhere, because the control plane
checks the placement, not the node's claim. A credential stolen out of a workload is usable from
anywhere until it expires, at most fifteen minutes, while the workload stays where it was; that
window is the cost of not making every API call round-trip to the node.

> Corrected 2026-10-13, when the credential was built. The first version said that moving or
> retiring the workload ends renewal, leaving a stolen credential good for the rest of its
> fifteen minutes. Authentication now looks up, with the credential, the workload's authoritative
> placement on the node it was issued to, so a move or a retirement ends every credential issued
> for the old placement at once, at the cost of that lookup on every call a workload makes.

**Where the credential is used.** Added 2026-10-13, while building the extension SDK, from the
algebra question: a workload holding a credential still had no way to learn where the control plane
is or how to trust its certificate, and its node is the one party that knows both. So the socket
answers with the credential, the control plane's address as the node reaches it, and the CA the
node pinned at enrollment (§3 of `18-operations.md`). Whether that address is reachable from inside
the workload is the workload's network's: §6 of `07-networking.md` opened it for an installed
extension and for nothing else by default, until the relay below replaced the opening (Finding 57
of `23-walkthroughs.md`). An installed extension's workload reaches the address through its node,
and the address still names the control plane to its TLS.

**Why not a secret in the intent.** A token written into the workload's configuration would be
stored in the database in the clear, re-delivered on every placement, and readable by anyone with
`workload.secret.read`; it would also survive the workload's move to a node that should no longer
be trusted with it. Node attestation has none of those properties, and it is the same trust
already placed in the node.

### 9.6 Signing in

> Designed 2026-10-16, for the web client of Phase 1 (§4 of `20-roadmap.md`). §9 listed the
> factors and §9.1 to §9.4 how authority and sessions begin and end; nothing said what an account
> is, how a person comes to have one, or what a sign-in checks.

**An account is a `User`** (§3.2 of `01-model.md`): an identifier, a display name its issuer gives
it, a sign-in name its holder chooses when redeeming its invitation, and the factors registered to
it. The sign-in name is what a password sign-in names the account by, unique in the installation
and lowercase, three to sixty-four of `a-z`, `0-9`, `.`, `_`, and `-`; a passkey sign-in needs
none, since the authenticator offers the account. It holds no authority by being an account;
everything it may do is the grants issued to `user:<id>`, exactly as for any other subject.

**A person comes to have an account by redeeming an invitation.** Issuing a grant to a new user
is the same call as issuing one to a new token (§3.7): it creates the account, holding the grant
and no factor, and an invitation for it, a random 256-bit secret shown once, of which the control
plane keeps the digest. Redeeming the invitation in the panel registers the account's first factor
and voids the invitation, in one transaction. An invitation expires after seven days unless its
issuer says less, and the bootstrap token of §9.1 is an invitation for the account holding the
root grant that expires in an hour. An account whose invitation lapsed holds its grants and has no
factor, so no one can sign in as it; its issuer sees it as invited and never joined, and may issue
it another invitation or revoke what it holds.

> Corrected 2026-10-16, when accounts were built. The first version created the account and issued
> its grant when the invitation was redeemed, which left the grant's checks to be run a second time
> at redemption against a parent that might since have changed. Creating both when the grant is
> issued, as a new token is, runs every check once, where it always runs, and shows the grant in
> its parent's tree from the moment it exists; the cost is an account with no factor for as long
> as its invitation is outstanding, which can do nothing.

**There is no open registration by default**: an account with no grants can do nothing, and a
sign-up form that creates accounts nobody asked for is only an attack surface. An operator selling
to the public turns it on (`--registration open`), and an account created that way holds nothing
until a grant is issued to it, by the host's billing system through the API, for example.

**Factors.** A **passkey** (WebAuthn, a discoverable credential with user verification required)
is a complete sign-in by itself, because it is something held and something known or been, and
it satisfies `requires_mfa` (§9). A **password** is never a sign-in by itself: it is accepted only
with a second factor, a passkey, a TOTP code, or a recovery code, so an account with a password
always has one. **TOTP** (RFC 6238, SHA-1, six digits, thirty seconds, one step of skew, each
code accepted once) is second class, as §9 says. **Recovery codes** are ten, each used once,
shown once at enrolment (§9.3). Passwords are stored as Argon2id with the parameters OWASP
recommends (19 MiB, two passes, one lane), and refused if shorter than twelve characters; TOTP
secrets are sealed by the control plane's vault, as every secret field is.

**The relying party is the control plane, and its identifier is the site the panel is reached at.**
A passkey is bound to a registrable domain, and the panel and the control plane share a site (§1 of
`11-surface-web.md`), so the identifier is that site's registrable domain: the control plane checks
that the browser signed for it and that the origin it reports is one it serves or lists in
`--web-origin`. In Phase 1 that is the installation's own name. A reseller's customers signing in
at the reseller's address need the control plane to answer under that address, which Phase 2
designs (Finding 66 of `23-walkthroughs.md`); a passkey then works at the site it was registered
at, and a person who signs in at two sites registers one at each. It accepts ES256, EdDSA, and
RS256, which is what platform authenticators produce, and attestation `none`, since the
installation has no list of authenticator models to trust and asking for attestation would only
identify the user's device to it.

**What a sign-in is limited by** (§10 of `10-api.md`). Every failure, a password, a second-factor
code, or an assertion that does not verify, is charged to its source like any failed attempt. A
password or a code can be guessed, so a failure also counts against the account, after OWASP's
device cookies: a browser that has signed in to the account holds a cookie the control plane
signed, naming the account, and its failures count against that cookie alone, which after five
in a quarter of an hour stops being honoured for a quarter of an hour; a browser without one
counts against the account's untrusted count, which after ten in a quarter of an hour refuses
every browser without a cookie for a quarter of an hour. A stranger can therefore keep an
account's holder from signing in from a new browser, and never from one they have used before. A
passkey assertion cannot be guessed, and its failures count against the source alone.

> Corrected 2026-10-16, when sign-in was built (Finding 69 of `23-walkthroughs.md`). The untrusted
> count is kept per sign-in name, whether or not it names an account, so it says nothing about
> which names exist, and as a bucket: ten failures, then one every ninety seconds, the same ten a
> quarter of an hour but recovered a guess at a time rather than refused for the quarter and then
> forgiven at once. Both it and the source's bucket are asked before a password is checked, and
> once either refuses the password is not checked at all, the right one included; checked first,
> as every other credential is, a refusal only told a guesser that the guess was wrong. Both
> buckets are charged before the check and given back when a sign-in succeeds, so guesses sent at
> once cannot all pass them (Findings 70 and 71). Until the device cookies arrive with the web
> sessions of §9.4, every browser counts as one without a cookie. (They arrived the next day, in
> the note below.)

> As built, and corrected, 2026-10-17. A redemption or a sign-in from a browser sets a device
> cookie, `__Host-korpis_device_` followed by the account's id, so a browser holds one for each
> account it has signed in to. Its value names the account and a device id of its own, signed by
> the control plane's token key under a purpose label of its own, so it cannot be presented as an
> access token nor an access token as it, and it is honoured for 180 days; each sign-in gives a
> new one. A password sign-in from a browser holding one for the account its name names is
> charged to the device's bucket, five guesses and then one every three minutes, and to a bucket
> every device of the account shares, ten and then one every ninety seconds, instead of to the
> name's. The name is looked up only once a cookie this control plane signed is presented, so a
> browser without one learns nothing from how long the answer takes. Once either of the device's
> buckets refuses, its cookie is not honoured and the guess is charged to the name's, as from a
> browser without one. The design above said five failures in a quarter of an hour stop a cookie
> being honoured for a quarter of an hour; it is a bucket for the reason the name's count is, the
> same five a quarter of an hour recovered a guess at a time. The shared bucket is not in OWASP's
> design and is there because each sign-in gives a new device: without it, someone who once knew
> the password could sign in again and again, keep a cookie from each, and after the password
> changed guess at its successor five times a quarter of an hour for every cookie kept; with it,
> every cookie of an account together has the name's budget, which only a browser that has signed
> in can spend. What it costs: such a former holder of the password can spend the shared bucket
> and send the holder's own browsers back to the name's bucket, which a stranger can spend too,
> so an account whose password someone else once knew is as exposed as one with no device
> cookies until theirs expire or the token key is rotated (§8 of `03-state.md`), which ends every
> device cookie as it ends every access token, and makes every browser one without a cookie
> until it signs in again. A passkey is still never slowed, and a CLI session (§9.4) is given no
> device cookie, having no browser to keep one.

> Corrected the same day (Finding 73 of `23-walkthroughs.md`, from a security review of the
> commit that built the cookies). The cost above was larger than it said: a device cookie
> outlived the password it was earned with, so whoever had once signed in kept, after the holder
> changed the password, the shared bucket on top of the name's, a budget a stranger never has,
> until the cookie expired 180 days later. A change of the password or of TOTP, the secrets a
> guess is aimed at, now stops every device cookie issued before it being honoured; the account
> keeps when they last changed, and a cookie is compared to it in whole seconds, which is all its
> expiry carries. The browser that made the change is given a new cookie in the same answer. What
> it costs: every other browser the holder uses is a browser without a cookie until it signs in
> again, so a stranger spending the name's guesses in that window keeps those browsers out too.

> As built, 2026-10-16. Two rules hold after every change an account's holder makes to its
> factors: something can still sign in, a passkey or a password; and a password has a lasting
> second factor beside it, a passkey or TOTP, because recovery codes run out and an account whose
> password outlived its last one could not sign in. Each change locks the account first, so two
> removals racing are decided in turn and cannot leave it with neither. Every single-use thing is
> spent by a conditional write: an invitation, a TOTP step (the step that confirmed enrolment
> included), a recovery code, a passkey's counter, and a challenge, which the answer naming it
> takes in a transaction of its own, so a refused answer spends it as surely as an accepted one.
> A sign-in refused for any reason is the same refusal, and a password naming no account is
> checked against a stand-in hash with today's parameters, so neither the answer nor its timing
> says whether a sign-in name exists. A passkey answering with a counter that did not advance is
> refused and recorded as `account.passkey_cloned`, naming the account and the passkey;
> invitations, redemptions, sign-ins, and factor changes are effects too (`account.invited`,
> `account.redeemed`, `account.signed_in`, `account.factor_added`, `account.factor_removed`,
> `account.recovery_codes_renewed`). The relying party is configured by `korpis-server serve
> --public-url`, the address the installation is reached at, whose host is the identifier unless
> `--rp-id` names a registrable suffix of it, and whose origin, with each `--web-origin`, is an
> origin a ceremony may come from; each must lie inside the identifier, or the control plane
> refuses to start, and without `--public-url` it says at start that no passkey can be used. It
> does not consult the public suffix list, so an `--rp-id` that is a public suffix is not refused
> at start; the browser refuses every ceremony for it instead. Passwords are hashed off the
> threads that serve requests, at most as many at once as the machine has cores, so a burst of
> sign-ins costs time rather than memory: at 19 MiB each, a sixteen-core machine spends at most
> about 300 MiB on them.

> As built, 2026-10-17. A holder changes their own factors through `ProfileService`, called with
> the access token of a session (§9.4): passkeys added and removed, the password set and removed,
> TOTP enrolled and removed, recovery codes renewed. Each change needs a recent sign-in: the
> session signed in, or re-authenticated, within the last ten minutes, or the change is refused
> with the reason `session.not_recent` and the panel asks for a factor first. A session
> re-authenticates with a passkey of its own account, answering a challenge of the sign-in
> ceremony, or with the account's password and a code beside it, charged and limited exactly as a
> sign-in is (§10 of `10-api.md`), and is recorded as `session.reauthenticated`. This follows
> OWASP ASVS 3.7.1 (`research/evidence.md` §5): without it, a session left open on a borrowed
> machine, or an access token copied from one, could replace the factors and keep the account
> after the session ended, and the factors are what a holder uses to end a takeover. A caller
> without a session, an API token held by a user, cannot re-authenticate and changes no factor.
> Reading the account and its factors needs no recent sign-in. Like the ceremonies, a change
> carries no idempotency key (§6 of `10-api.md`). What it costs: a holder who signed in more than
> ten minutes ago answers one more ceremony before a change, and one who signs in with a password
> and a recovery code spends a second code to re-authenticate.

What it costs. The holder of an account who has lost every browser and every factor gets back
in only through an administrator (§9.3) or, for root, through the machine (§9.2): the price of
no email reset. An operator who turns on open registration answers for the accounts it creates.
And the relying party is Korpis's own code, on Rust cryptography already in the tree, because the
established WebAuthn libraries for Rust are either licensed on terms this project does not take
into its dependencies or maintained by one person on a release cadence it cannot depend on; it is
tested against a software authenticator and a browser's virtual one (§9 of `21-stack.md`).

---

## 10. Open questions

1. **Token format. Resolved here: Biscuit.** Biscuit gives offline attenuation and offline
   verification with a public key, in a specification designed for exactly this. Macaroons are
   simpler and older but their third-party caveats are awkward. A custom format is more work and
   fewer surprises. This choice determines whether a *client* can attenuate a token without
   contacting the control plane, which would make §6.1 links issuable entirely offline.

Biscuit, for three reasons that are now load-bearing. Its attenuation appends signed blocks that can
only restrict, which is the algebra of §3.2 expressed cryptographically, including the delegation
policy, so an offline verifier checks every rule of §3.2 from the token alone. Its verification is
public-key, so relays and agents hold nothing secret. And it has an open specification with
independent implementations, which K-7 needs from anything a third party must verify. The cost is
Datalog evaluation in the verifier, bounded by Biscuit's own execution limits, and a format whose
evolution Korpis does not control, which is why tokens carry their format version and the verifier
negotiates it like any other capability.

Client-side attenuation is permitted, and it is not a loophole: a client attenuating offline can
produce only what the chain's delegation policy already allows, and a child minted offline is
unknown to the store, so it can be used only for actions that do not need a counter or an approval
(§6.2), and it is revoked by revoking any link above it.
2. **Are teams a subject, or just nested organizations?** A "team" that spans projects but is not
   an organization does not exist in the model. Nested organizations cover it awkwardly, a team is
   not a tenancy boundary and does not want a quota. Adding a `Group` subject is simple and adds a
   second grouping concept alongside labels. → here
3. **Organization transfer.** Selling or handing over an organization moves it under a new parent.
   Every grant issued by the old parent must be re-validated against the new one, and some will
   fail attenuation. Whether that revokes them, suspends them, or blocks the transfer is
   unresolved, and it is exactly the operation a hosting business performs when a reseller changes
   hands. → here
4. **Quota consumption by revoked grants. Resolved here.** A revoked grant's workloads keep running
   and keep consuming the tenant's quota. Correct, revoking someone's access should not delete their
   work, but it means quota can be consumed by resources nobody can currently manage. Who reclaims
   it, and after how long? It was routed to `05-scheduling.md`, which never carried it (finding
   S18).

The premise does not hold. Workloads belong to a project and an organization, never to a grant, so
revoking a person's grant leaves the workload where it was, managed by whoever holds authority over
that organization. There is always someone: every organization is beneath the root, and the
non-private actions (stop, delete, quota) flow down through containment (§3.3). Quota is therefore
never consumed by something nobody can manage. When an organization's own administrators are all
gone, its parent can stop or delete its workloads on the record, and can read their data only by
the explicit, tenant-visible grant of §3.3.
5. **Effect-log visibility across the tenancy tree.** A parent can see that a child's workload was
   restarted. Can it see which of the child's users did it? Operationally useful for support,
   uncomfortable for privacy, and the answer probably differs between a hosting provider and a
   community. → here
6. **Rotating a token's secret without reissuing its grant.** A token issued to hold a grant
   (§3.7) cannot have its secret replaced; a suspected leak is answered by revoking the grant and
   issuing another, which changes the grant's identifier that effects cite. A rotation that keeps
   the grant would keep the audit trail continuous, and needs a rule for how long the old secret
   keeps working while a pipeline is updated. → here
7. **Derived suspension, offline.** A suspended organization (§2.1) and an unverified binding
   (§5.1) suspend grants by derivation, and nothing writes a row per grant, so the revocation list
   (§6.3), which lists grants whose own row is revoked or suspended, does not refuse their tokens
   offline; they stop at renewal, within one token lifetime. Either the control plane expands a
   suspension into the grants it suspends when it computes the list, which is a walk the
   derivation exists to avoid, or the list also names suspended organizations and bindings and a
   verifier refuses a token whose chain is rooted in one or whose resource lies beneath one, which
   needs the root's identity in the token and the resource's ancestry at the verifier. Settled with
   organization suspension and identity bindings, which Phase 2 builds (§4 of `20-roadmap.md`).
   → here
