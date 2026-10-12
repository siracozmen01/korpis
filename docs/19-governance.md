# Governance

**Status:** design **Date:** 2026-08-07 **Depends on:** [`00-overview.md`](./00-overview.md),
[`10-api.md`](./10-api.md) **Implements:** Rules K-8, K-13, K-19; Principle P10

---

## 1. Pterodactyl's failure was not technical

This document exists because of a specific finding in `research/evidence.md`, and it is worth
stating without softening: **the architectural problems Korpis was designed around were survivable.
The governance problems were not.**

- A v2 rewrite was announced and abandoned.
- Issue #141 has been open since October 2016: not rejected, not explained, open.
- Correctness reports were closed as *not planned*, including a file 858 times larger than its
  declared limit.
- Decision-making concentrated to the point where the project's velocity was one person's
  availability, and when that became untenable, the outcome was a community fork rather than a
  transition.

Every one of those is a process outcome. K-8 therefore makes license, versioning, and
multi-maintainer governance **day-one decisions**, settled here, before the first release, because
a project that defers governance until it has users is deferring it until the decision is
contested.

---

## 2. License

Settled in §5 of `00-overview.md` and restated because this is where it belongs:

| Layer | License |
|---|---|
| Protocol definitions, client SDKs, recipe format, agent protocol | **Apache-2.0** |
| Control plane, node agent, web/Discord/CLI clients | **AGPLv3** |
| Recipes, extensions, themes | Author's choice, including proprietary |

### DCO, not a CLA

Contributions are accepted under a **Developer Certificate of Origin**. There is no contributor
licence agreement, no copyright assignment, and no entity holding the right to relicense the
project.

This is deliberate and it deliberately costs something. A CLA concentrates relicensing power in one
organization, and that concentration is the mechanism by which open projects are later taken
closed, a different route to the outcome MIT permitted for Pterodactyl, and one that has been
walked by several well-known projects since. Distributed copyright makes a future relicense require
broad consent from contributors.

**Stated plainly: this forecloses an open-core pivot.** Korpis cannot later gate features behind a
paid edition, because doing so would require the agreement of everyone who ever contributed. P10
and K-13 are not promises in a README that a future board can revise; they are enforced by the
licence structure.

### Forks are welcome

Fork it, modify it, run it, sell hosting on it. The AGPL obligation arises only if you modify
Korpis itself and offer the modified version over a network, in which case you publish the
modifications. Using it unmodified obliges nothing at all.

**Hosts are not blocked, restricted, capped, or charged. That is the entire point of P10**: no
per-workload, per-node, per-instance, per-account, or per-domain anything, at any scale, forever.

---

## 3. Trademark

The name and marks are held separately from the code, and this is what makes §2's permissiveness
safe rather than reckless.

- Unmodified builds may use the name and marks freely.
- **Commercial hosting under the Korpis name is explicitly permitted.** A host saying "we run
  Korpis" or "Korpis hosting" needs no permission and owes nothing.
- A **modified** distribution must be renamed. Not because modification is discouraged, §2 welcomes
  forks, but because a user who downloads something called Korpis must get Korpis, and security
  advisories must refer to a knowable artifact.
- The marks are never used to restrict use, competition, or resale. They exist to prevent
  confusion, and any enforcement outside that purpose is a violation of this policy by whoever
  attempts it.

---

## 4. Versioning

Semantic versioning, with the public surface defined precisely, because "SemVer" without that
definition is an argument waiting to happen:

| Public API: breaking changes require a major version | Not public API |
|---|---|
| the agent protocol | the database schema |
| the client API schema | internal package layout |
| the recipe format and install DSL | the web client's appearance and layout |
| the extension contract | log message wording |
| the CLI's `--json` output and exit codes | the CLI's human-readable output |
| storage-class and driver capability names | performance characteristics |

The action vocabulary (§5 of `10-api.md`) sits in the first column with an extra rule attached:
**actions are never renamed or split**, at any version, including a major one, because a grant
issued under `v1` is a delegation someone made deliberately and a major version is not permission
to reinterpret it.

---

## 5. Releases and LTS

**Time-based, not feature-based.** A minor release on a fixed cadence (target every eight weeks),
containing whatever is ready. Feature-based release dates are how a v2 gets announced and
abandoned; a train that leaves on schedule cannot be held hostage by one incomplete feature.

**An LTS annually, supported for twenty-four months** with security and correctness fixes.

LTS is not a courtesy here, it is a requirement derived from who runs this software. A host with
sixty nodes and paying customers cannot re-qualify a platform six times a year, and the market's
incumbents charge for exactly this stability. Under P10 it is free.

The LTS commitment constrains the protocol, which is the useful part: **an LTS control plane must
be able to speak to agents for its entire supported life**, so the agent-protocol window of §4 of
`10-api.md`, which is defined by what is supported rather than by a count of versions, is not an
abstract policy, it is a dated obligation that makes a proposed protocol change expensive
in a way that shows up during review rather than afterwards.

### Deprecation

Announce in a release note → warn in-band in API responses and CLI output → remove, no sooner than
the next LTS. Nothing is ever removed without having first warned in the channel where it is used.
A deprecation that only appears in release notes is a deprecation nobody saw.

---

## 6. Maintainership, and the bus factor

The rules below exist because "the maintainer got busy" is the documented cause of the failure in
§1, and no amount of code quality compensates for it.

**Two maintainers with independent, complete rights from the first commit.** Not a lead and a
helper. Two people who can each cut a release, sign an artifact, and merge to a release branch, and
neither of whom can do so alone:

> **No single person merges to a release branch.** Every release-branch change requires a second
> maintainer's approval, including the founder's, including trivial changes, including during an
> incident. An exception process for security fixes exists, is time-boxed, and requires
> retrospective review by the second maintainer.

**Infrastructure ownership is written down and is not one person's account.** This is the real bus
factor, not code review, but who can log in:

| Asset | Requirement |
|---|---|
| domain registration | organization-held, at least two people with access |
| registry namespaces (recipes, extensions, images) | organization-held |
| release signing keys | split or escrowed; no single-holder key |
| CI and build infrastructure | reproducible from configuration in the repository |
| package repositories | organization-held |
| the source repository organization | at least two owners |

A project whose signing key exists on one laptop has a bus factor of one regardless of how many
maintainers review pull requests.

**Succession is documented before it is needed.** How a maintainer is added, how an inactive one
moves to emeritus, and what happens if everyone with access is unreachable at once. Maintainers are
added for sustained contribution and judgement, not for volume.

**No foundation initially.** Standing one up before there is anything to steward is expensive
ceremony. But the assets above are held so that transferring them to a neutral entity later is a
transfer rather than a negotiation, which is the other reason there is no CLA.

---

## 7. Decisions, and the cost of silence

Changes to **the domain model, the protocol, the security boundary, the licence, or the governance
rules** require a written proposal, a comment period, and an explicit decision. Everything else
runs on lazy consensus.

The rule that matters most is about the decisions that are never made:

> **Silence is not a decision, and an untriaged issue is a process failure rather than a backlog
> item.** Every issue receives a triage response within a published window. "We are not going to do
> this, because, " is a good outcome. Nine years open is not.

Issue #141 was not harmed by being declined. It was harmed by never being answered.

**Closing as *not planned* requires a stated reason.** Closing a *correctness or security* report
as not planned requires **two maintainers and a written rationale**, because that is precisely what
happened to the 858 GB-over-quota report, and it is the single clearest example in the research of
a process producing a technical outcome nobody would have chosen deliberately.

The roadmap is public, dated, and says what is *not* being done, §2 of `00-overview.md` is the
permanent version of that, and per-release scope is the temporary one.

---

## 8. Contribution

The conformance suites are the contribution gate and the reason this scales past the founding
maintainers. A runtime driver is accepted if it passes the driver suite (§8 of `04-runtimes.md`);
an agent implementation is conforming if it passes the agent suite (§11 of `10-api.md`). Neither
requires a maintainer to hold the whole system in their head to review, which is the bottleneck
that throttles projects of this size.

Documentation lives in the repository and changes with the code. A behaviour change that does not
update the document describing it is incomplete, and the twenty-four documents in this directory
are the specification, not a description written afterwards.

Security disclosure has a published contact and a stated response window (§12 of `17-security.md`).
Advisories are published with the same detail Korpis' own research demanded of others: what was
reachable, what it affected, which versions, and what the fix changed.

---

## 9. Money, stated honestly

This is the largest unresolved risk in the project and it is not a technical one.

P10 forbids taxing growth and §2 forecloses open-core, which together remove every revenue
mechanism the commercial products in this market use, per-server, per-machine, per-instance,
per-account, and per-domain licensing, and reselling restrictions below a top tier. That is the
point rather than an oversight: Pterodactyl took this market by removing the tax, and imposing one
forfeits the only durable advantage a new entrant has.

What remains: donations and sponsorship; paid support, consulting, and managed hosting offered by
anyone including maintainers, as separate businesses; and funded development of specific features
by parties who want them, delivered into the open project.

**Several projects have died in exactly this position, and pretending otherwise would violate P4 in
the one document where the honesty matters most.** The mitigations are structural rather than
hopeful: costs stay low because Korpis operates no infrastructure (no marketplace, no registry, no
telemetry backend, no hosted service) and the maintainer rules in §6 mean the project can survive
any individual's economics changing.

---

## 10. Open questions

1. **Foundation timing.** §6 defers it and keeps the option open. What triggers it (contributor
   count, corporate adoption, a maintainer's need to divest) is unstated. → here
2. **Recipe ecosystem governance.** Recipes are OCI artifacts anyone can publish (§8 of
   `16-extensions.md`) and Korpis is not a marketplace. Whether a *reference* set is maintained by
   the project, and who decides what enters it, is unresolved and is the seed of a curation
   problem. It was routed to `09-recipes.md`, which never carried it, while §9 of
   `22-first-party.md` routed the same question here (finding S18). It is a governance question and
   it stays here. Part of it is settled: a first-party recipe is one the project maintains, which
   means one with a named maintainer, so the first-party set can never be larger than the
   maintainers can keep current, and a recipe whose maintainer steps away is relabelled community
   rather than left to rot under the project's name. Who adds to it beyond that is open. → here
3. **Funded-development boundaries.** A sponsor paying for a feature that conflicts with the
   principles is a governance test that will arrive, and the answer needs to be written before the
   money is on the table rather than during the conversation. → here
4. **Translation governance.** Community translation spread Pterodactyl; it also means strings in
   many languages that no maintainer can review. The mechanism is settled in §10 of
   `11-surface-web.md`; who may approve a translation into a language no maintainer reads is a
   governance question and stays here (finding S18). → here
5. **LTS security backport scope.** Twenty-four months of "security and correctness" needs a line
   between a correctness fix and a behaviour change, or the LTS branch drifts into a second
   product. → here
