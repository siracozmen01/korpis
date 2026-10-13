**English** · [Türkçe](./README.tr.md)

# Korpis

**A workload platform for machines you control. Design specification.**

Korpis runs anything with a lifecycle (a game server, a bot, a web service, a database, a scheduled
job, a virtual machine) on hardware you own. It places the workload on a node, isolates it as
strongly as you ask, gives it storage and network, keeps it in the state you declared, and lets you
and the people you delegate to operate it from a panel, a terminal, chat, or the API.

> **The design comes first, and the code has finished its first phase.** Phase 0 of the build
> order is built in `crates/` (Rust, §3 of [`21-stack.md`](./docs/21-stack.md)): a workload
> declared through the CLI is placed, run, and audited end to end, which
> [`scripts/e2e-phase0.sh`](./scripts/e2e-phase0.sh) checks. It is not yet usable as a panel; the
> web client, backups, and public endpoints arrive in Phase 1.
>
> Twenty-four documents, and they are the specification the code is held to. Some decisions in a
> system like this cannot be retrofitted: how state is stored, how authority is delegated, where the
> security boundary sits, what the agent protocol promises. Getting those wrong on paper costs a
> paragraph. Getting them wrong in month nine costs a rewrite, which is a thing that has already
> happened to this category of software more than once.

---

## What it does differently

**Declares, never commands.** You say what should be true. Korpis converges reality toward it,
continuously, and reports what is actually true. No component tells another to do something and
assumes it worked, which is where "the panel says stopped but the server is running" comes from.

**Makes the diff a real object.** Every change produces an inspectable `Plan` before it produces an
`Effect`. Dry-run, approvals, scheduled changes, rollback, and an audit trail that cannot lie are
consequences of that single decision rather than five separate features.

**Has no roles.** Authority is a *grant* (a subject, some actions, a scope, some conditions) and a
grant can only ever produce weaker children. So "this link lets your friend restart this one server
for the next 24 hours, no account needed" is an ordinary operation, and reselling is something the
model does for free rather than a feature anyone writes.

**Lets you choose the isolation.** `process`, `container`, `microvm`, `vm`, per workload, behind
one driver interface. Each tier is documented by what it does *not* stop as much as by what it
does, because a tier that looks like isolation and isn't is worse than one honestly labelled
weaker.

**Never claims what it doesn't enforce.** Every displayed limit is enforced by the kernel and
metered. Every status is observed. When Korpis can't see something it says `unknown` instead of
guessing, and when it drops data it leaves a marked gap instead of a clean-looking hole.

**Doesn't tax growth.** No per-workload, per-node, per-instance, per-account, or per-domain charge,
at any scale, ever. No editions, no gated features, no commercial-use restriction.

---

## Start here

**[`00-overview.md`](./docs/00-overview.md)**: what Korpis is, what it deliberately is *not*, ten
principles, four falsifiable bets, and eighteen binding rules, twelve of them derived from evidence.

Then whichever of these you actually care about:

| Question | Document |
|---|---|
| Does the model hold together? | [`01-model.md`](./docs/01-model.md) · [`02-architecture.md`](./docs/02-architecture.md) · [`03-state.md`](./docs/03-state.md) |
| Is it safe? | [`17-security.md`](./docs/17-security.md) · [`04-runtimes.md`](./docs/04-runtimes.md) · [`08-identity.md`](./docs/08-identity.md) |
| **Does it survive contact with reality?** | [`23-walkthroughs.md`](./docs/23-walkthroughs.md): fourteen scenarios traced end to end, the defects they found, and two external reviews |
| Could I run it? | [`18-operations.md`](./docs/18-operations.md) |
| Could I build on it? | [`10-api.md`](./docs/10-api.md) · [`16-extensions.md`](./docs/16-extensions.md) · [`09-recipes.md`](./docs/09-recipes.md) |
| What gets built first? | [`20-roadmap.md`](./docs/20-roadmap.md) |
| Who decides things? | [`19-governance.md`](./docs/19-governance.md) |
| Why any of this? | [`docs/research/evidence.md`](./docs/research/evidence.md) |

Full map in §7 of the overview.

```
README.md CONTRIBUTING.md SECURITY.md LICENSE*  entry points, review policy, disclosure, licensing
CLAUDE.md                                       the rules anyone editing these documents works under
docs/00-overview.md .. docs/23-walkthroughs.md  the specification, numbered in dependency order
docs/research/evidence.md                       the citations every deviation is argued from
crates/                                         the implementation, a Rust workspace
ARCHITECTURE.md                                 how the code is layered, and why
```

**The open questions are the point, not the gaps.** Fifty-nine are open across the documents, each
one filed in the document that will settle it. A deferred decision with a name on it is
honest. An undeclared one is a trap someone walks into later.

---

## How this was written

**By one person, with Claude.** That is worth saying plainly rather than leaving for someone to
notice.

Nobody should take a specification on trust because of who or what produced it, so here is what was
actually done to make it worth reading:

- **Every deviation is argued from something citable, or says it is a decision.** Rules K-1 to K-12
  in §6 of the overview each trace to a published advisory, a specific issue, or documented product
  behaviour, collected in [`docs/research/evidence.md`](./docs/research/evidence.md) with
  confidence tags. K-13 to K-18 are policy and design decisions, and that file states what each
  rests on. Sources that turned out to be SEO content are marked as such and not relied on.
- **The design was attacked on purpose.** [`23-walkthroughs.md`](./docs/23-walkthroughs.md) traces
  fourteen concrete scenarios across every layer they touch, looking specifically for the step
  where no document is holding the ball. It found twenty-three defects, including a quota race,
  revoked authority that could resurrect after a restore, an edge that could stop forwarding while
  every observation reported healthy, and a chat role whose removal cascaded to nothing. All are
  fixed, and the walkthrough documents them rather than quietly patching them.
- **A second reading attacked the definitions rather than the seams.** An independent review by a
  different model went after the algebra inside each document: what evaluates this condition, with
  what information, and can it. It found nineteen things, seventeen of them real, including a limit
  the identity document displayed that no verifier could enforce. All are patched, and §16 of
  [`23-walkthroughs.md`](./docs/23-walkthroughs.md) records every one along with the two claims
  that did not survive checking.
- **Then the whole thing was read again, against itself and against its sources.** A second
  review read every document against every other and checked every claim about another project
  against the primary source. It found seventy-nine things: protocol mechanics that earlier fixes
  had left half-applied, a data path that contradicted the dial-out design, and nine wrong
  citations in the evidence file itself, including an issue described as unanswered that had been
  answered the same day. §17 of [`23-walkthroughs.md`](./docs/23-walkthroughs.md) records each
  one and where it was fixed.
- **Corrections are left visible.** §2 of [`01-model.md`](./docs/01-model.md) is a record of an
  earlier proposal in this repository being wrong and why. Nothing is presented as having been
  obvious.

The judgement calls, the priorities, and the mistakes are the author's. If something here is wrong,
it is wrong on its own terms. Open an issue and say so.

---

## Contributing

The most useful thing anyone can do right now is **argue with it**. A specification that has never
been disagreed with has not been reviewed.

Especially wanted:

- Somewhere the reasoning doesn't hold, or an assumption that isn't stated
- An operational failure mode the walkthroughs missed
- Anything in [`docs/research/evidence.md`](./docs/research/evidence.md) that is inaccurate or
  unfair. Accuracy matters more here than being right
- Experience running this class of software at scale, which no amount of design substitutes for

See [`CONTRIBUTING.md`](./CONTRIBUTING.md). Security policy: [`SECURITY.md`](./SECURITY.md).

---

## License

| Layer | License |
|---|---|
| Protocol definitions, client SDKs, recipe format, agent protocol | **Apache-2.0** |
| Control plane, node agent, relay, web / CLI / chat clients | **AGPL-3.0-or-later** |
| These documents | **CC BY 4.0** |
| Recipes, extensions, themes | Author's choice, proprietary included |

Plainly, because AGPL gets misread as a restriction on use: **run it commercially, sell capacity on
it, charge what you like, build closed extensions against it.** Running it unmodified for your
customers obliges nothing. Modifying it and offering the modified version over a network means
offering its users your source; distributing copies means the source goes with them, which an
unmodified build already arranges.

Contributions come in under a Developer Certificate of Origin, with no CLA and no copyright
assignment, so nobody can relicense a contributor's code without them. The licence does not by
itself rule out an open-core pivot; the project's policy (P10, K-13) does, and §2 of
[`19-governance.md`](./docs/19-governance.md) says which is which.
