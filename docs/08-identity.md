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
Organization                ⊑  any ancestor Organization
Selector(P, expr)           ⊑  P            for any expression
```

The last line resolves open question 3 of `01-model.md`.

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
- **An approver for S is a subject holding `plan.approve` through a grant whose scope contains S.**
  The set of approvers therefore shrinks as S grows: approval scoped to an organization can be given
  only by someone with authority over the whole organization.
- **"At least as strict" means equal or larger.** `child.requires_approval_by` must be set when the
  parent's is, and must contain the parent's: a child can move approval up the tree toward fewer,
  more senior approvers, never down toward its own holder.
- **Nobody approves their own plan.** The approving subject must differ from the proposing subject,
  and the approving grant's chain must not pass through the proposing grant, so minting oneself a
  child grant does not manufacture a second approver.
- The approval is an effect naming both subjects and both chains, and `requires_approval_by` is
  never cached (§5.1 of `02-architecture.md`).

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
volume, deleting a workload, releasing an address, and pruning backups. They are excluded unless an
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
  grant_chain     the attenuation chain, each link signed
  actions, scope, conditions
  issued_at, expires_at
  epoch           the issuing control plane's key epoch
```

The relays and the agents hold the control plane's public key and verify a token **offline**. This
is what makes interactive work possible without a round trip per keystroke:

```
client attaches to console
  → gateway evaluates the grant chain (online, authoritative)
  → issues a capability token: {workload.console.write on workload W, expires in 120s}
  → the client presents it to a relay, which verifies it offline and routes the session
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
| signature and grant chain, including every link's delegation policy | relay and agent, offline | **yes** |
| `expires_at`, `not_before` | relay and agent, offline | **yes** |
| revocation of any link | relay and agent, against the last revocation list, offline | **yes**, as of the last list received |
| `source_cidr` | the gateway for API calls; the relay for sessions; offline at the relay | **yes** |
| `requires_mfa`, `requires_approval_by` | gateway, online | no |
| **`max_uses`** | **gateway, online, redeemed atomically** | **no** |

A token carrying `max_uses` is redeemed at the gateway, which decrements the counter in the same
transaction that authorizes the call. It is therefore **not** usable while the control plane is
unreachable, and a token without `max_uses` is. That is the trade and it is the right way round:
the feature that needs a counter is the one that gives up offline operation, rather than every
token giving up a guarantee to support one condition.

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

### 9.2 When the last administrator is locked out

Whoever holds root on the machine running the control plane already holds everything: the
database, the signing key, the process. Pretending otherwise would be the kind of claim §8 of
`17-security.md` refuses. So recovery from a lost root grant is honest about where it comes from:
`korpis-server admin recover-root`, run locally by the machine's root user, issues a new root grant
to a newly registered passkey and writes an effect saying exactly that, which every tenant whose
data is in scope can see. It needs no network listener and no secret question, and it cannot be
done from anywhere except the machine.

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

Every live session is listed for its user with where and when it was created, and any of them can
be ended, including "end all others", which revokes the refresh credentials immediately and lets the
outstanding capability tokens expire within their lifetime. A session never outlives the grants it
exercises: each token is minted against current authority.

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
