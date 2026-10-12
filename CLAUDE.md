# Korpis

A general-purpose workload platform: its specification in `docs/`, and its implementation in
`crates/`, a Rust workspace built in the order of §4 of `docs/20-roadmap.md` (Phase 0 is under way).
**The documents are the specification and the code implements them.** When the two disagree, one of
them is wrong, and the fix lands in the same commit as the discovery: the code is corrected, or the
document is corrected in place with its reasoning.

The Rust code follows `rustfmt.toml` and the workspace lints in `Cargo.toml`, and every commit
passes `scripts/check.sh` (formatting, clippy with warnings denied, tests, the supply-chain policy
of `deny.toml`, and the text checks below). Every source file starts with its SPDX licence line:
`Apache-2.0` in `korpis-proto`, `AGPL-3.0-or-later` everywhere else (§2 of
`docs/19-governance.md`).

## Hard rules

These are not preferences. Breaking one means the change is wrong.

1. **No subagents.** This is the repository's first rule, set by the maintainer. An AI agent
   working here does the work itself, in its own session: it does not delegate reading, review,
   search, or editing to spawned agents or forks. A review is only as good as the one context that
   holds all of it, and every finding in §17 of `docs/23-walkthroughs.md` was checked by the agent
   that recorded it.
2. **Never use the em dash character.** Not in documents, commit messages, issue replies, or code
   comments. Restructure the sentence instead: a comma for an aside, a colon before an explanation,
   parentheses for a true parenthetical, a semicolon or a full stop between two independent
   clauses. A hyphen surrounded by spaces is not an acceptable substitute. The check is in "Before
   committing" below, and it must return nothing.
3. **All documentation is in English.** Discussion may happen in any language; what lands in the
   repo is English.
4. **Never use the name "Blueprint".** It is Pterodactyl's de facto extension framework. Korpis
   calls its package format `Recipe`. See §1 of `docs/09-recipes.md`.
5. **Do not reduce scope.** If something looks too large, say so and argue it, but do not quietly
   drop it, narrow it, or replace it with a simpler thing. Removing a decision from a document is a
   change that needs the same justification as adding one.
6. **Do not restrict hosts.** No per-workload, per-node, per-instance, per-account, or per-domain
   limit, charge, or gate, at any scale. This is Principle P10 and it is load-bearing for several
   other decisions.

## Layout

```
README.md CONTRIBUTING.md SECURITY.md            entry points, review policy, disclosure
LICENSE LICENSE-APACHE LICENSE-AGPL LICENSE-CC-BY the licence split and the full licence texts
docs/00-overview.md                              principles, non-goals, bets, binding rules
docs/01-model.md .. docs/23-walkthroughs.md      the specification, in dependency order
docs/research/evidence.md                        the citations every deviation is argued from
Cargo.toml crates/                               the Rust workspace, one crate per component
scripts/check.sh                                 every check a commit must pass
```

Numbering is dependency order, not importance. A document may depend on a lower number and should
avoid depending on a higher one; where it must, the dependency is stated in the header block.

## Which document owns which decision

Put a decision where it will be settled, not where it is first mentioned. If you are unsure, these
are the owners:

| Subject | Document |
|---|---|
| nouns, fields, lifecycles | `01-model.md` |
| control plane / data plane split, reconciliation, assignments, relays | `02-architecture.md` |
| Intent, Plan, Effect, storage engine, restore | `03-state.md` |
| runtime driver interface and isolation tiers | `04-runtimes.md` |
| placement, quota, cgroup limits, migration | `05-scheduling.md` |
| volumes, snapshots, backup, dedup scope | `06-storage.md` |
| endpoints, overlay, edge forwarding, egress | `07-networking.md` |
| tenancy, grants, delegation, capability tokens | `08-identity.md` |
| recipe format, install DSL, signing | `09-recipes.md` |
| schema, transport, versioning, compatibility | `10-api.md` |
| threat model, confinement, honest negatives | `17-security.md` |
| install, upgrade, HA, disaster recovery | `18-operations.md` |
| licence, versioning, LTS, maintainership | `19-governance.md` |
| build order and what is deferred | `20-roadmap.md` |
| implementation languages | `21-stack.md` |
| the first-party recipe and extension set | `22-first-party.md` |

## How documents are written

**State the decision, then the reasoning, then what it costs.** A section that gives only the
decision is incomplete. A section that gives no cost is usually hiding one.

**Cite the evidence.** When a claim rests on something that happened in another system, name it
with an issue number, advisory ID, or documented behaviour, and add it to
`docs/research/evidence.md` with a confidence tag. Sources that turn out to be marketing or SEO
content are marked as such and are not relied on.

**Say what a thing does not do.** The most valuable paragraphs in `17-security.md` and
`18-operations.md` are the honest negatives. An isolation tier is described by what it fails to
stop. A limit that is displayed but not enforced by the kernel must not be displayed at all (Rule
K-3).

**Record corrections in place.** When an earlier decision in this repository turns out to be wrong,
leave the correction visible with its reasoning rather than rewriting history. §2 of `01-model.md`
is the model for this.

**A correction reaches every document that restates the corrected claim.** Fixing the owning
document is half of a correction. Before committing one, search the whole repository (the
documents, the README and its translation, `CONTRIBUTING.md`, `SECURITY.md`, `LICENSE`, and this
file) for every restatement of the old claim, its numbers, and its section references, and fix each
of them in the same commit. Most defects the second external review found were a fix applied in one
document and left standing in three others.

**Open questions are numbered, live in a final section, and name where they will be settled.** The
form is a numbered item ending in `→ <document>` or `→ here`. A deferred decision with a name on it
is honest; an undeclared one is a trap. Resolving one means editing it in place to say `Resolved in
§N of X` and updating the owning document, not deleting it.

**Cross-references are section-precise.** Write ``§5.2 of `05-scheduling.md` `` rather than "see
the scheduling document". Verify the section number exists before writing it; guessing from memory
is the single most common defect in this repo's history.

**Line length under 100 characters.** Prefer prose to bullet fragments. Tables are for genuinely
tabular material, not for prose that has been chopped up.

## Translations

`README.md` is English and is the source. `README.tr.md` is a translation of it, and its first line
is an HTML comment naming the commit of `README.md` it was translated from. When you change
`README.md`, either update the translation in the same commit, or say in the commit message that
the translation is now behind. A commit cannot name its own hash, so when the two change together
the comment is updated to that commit in the one immediately after it.

Only the README is translated. The documents in `docs/` are English and stay that way. The agent
protocol, the rule names, and the model's nouns have to be defined in one language, or two
translations drift apart silently and there is no way to tell which one is the specification.

## Before committing

```bash
grep -rnP --exclude-dir=.git '\x{2014}' .     # em dash: must return nothing
grep -rn --exclude-dir=.git --exclude=CLAUDE.md -e '01-landscape' -e 'prior-art' .
                                              # removed material: must return nothing
```

The second command excludes this file because this file has to name the patterns; without the
exclusion it matches itself and can never return nothing.

Also check: every relative link resolves, no duplicated `## N.` heading number inside a file, and
the document map in §7 of `docs/00-overview.md` matches what is actually in `docs/`.

Commits are signed off for the DCO (`git commit -s`). There is no CLA. Commit messages say what
decision changed and why, not just which file moved.

## Things worth knowing

**This repository openly discloses that it was written with Claude.** The README says so in its own
section. Do not remove that, and do not add hedging around it either. The argument for taking the
work seriously is in the validation it describes, not in who typed it.

**Competitor pricing, feature matrices, and business-model analysis are deliberately not in this
repository.** `docs/research/evidence.md` keeps only technical citations: advisories, issue
numbers, and documented behaviour. Sourced criticism is engineering. The same criticism without
sources is something else, which is why the evidence file stays even though the market analysis
went.

**`docs/23-walkthroughs.md` is the test suite.** It traces scenarios end to end across every
document they touch, looking for the step where no document is holding the ball. Fourteen traces
produced twenty-three defects, and two external reviews (§16 and §17) found more. When a change
touches more than one document, add or update a trace.

**Traces and algebra review are different instruments, and a document needs both.** A trace
confirms that every step has an owner. It does not confirm that a step is possible. §16 of
`23-walkthroughs.md` records an external review that found seventeen defects the traces could not,
every one of them a document asserting a property its own definitions cannot support. So when a
change adds a field, a condition, or a limit, ask the algebra question of it directly: **what
evaluates this, where, and with what information?** The answer has to name a component that
actually holds that information. "The verifier checks it" is not an answer if the verifier is
offline.

Pick the next trace by **measuring coverage, not by intuition**. Count how many times each document
is cited by the existing traces; the second set of seven was chosen because `05-scheduling.md` had
been crossed seventeen times and `13-surface-cli.md` not once, and it found more defects than the
first set did.
