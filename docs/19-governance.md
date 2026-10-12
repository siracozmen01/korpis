# Governance

**Status:** design **Date:** 2026-08-07 **Depends on:** [`00-overview.md`](./00-overview.md),
[`10-api.md`](./10-api.md) **Implements:** Rules K-8, K-13; Principle P10

---

## 1. Pterodactyl's failure was not technical

This document exists because of a specific finding in `research/evidence.md`, and it is worth
stating without softening: **the architectural problems Korpis was designed around were survivable.
The governance problems were not.**

- A v2 rewrite was announced and abandoned.
- The control-plane/daemon contract was never written down. Asked in 2016 for third-party daemon
  support (#141), the answer was that any daemon following "the same API style as the core daemon"
  would work: the contract was whatever the reference implementation did.
- Correctness reports were closed as *not planned* without a stated reason, including a file 858
  times larger than its declared limit, closed in a batch of 86 issues on one day.
- Decision-making concentrated to the point where the project's velocity was one person's
  availability, and when that became untenable, the outcome was a community fork rather than a
  transition.

Every one of those is a process outcome. K-8 therefore makes license, versioning, compatibility, and
maintainership **day-one decisions**, settled here, before the first release, because
a project that defers governance until it has users is deferring it until the decision is
contested.

> Corrected 2026-10-12. The second bullet said #141 had been open since 2016, "not rejected, not
> explained, open". It was answered the same day and closed three days later (§1.6 of
> `research/evidence.md`). The finding it supports is the one stated now.

---

## 2. License

Settled in §5 of `00-overview.md` and restated because this is where it belongs:

| Layer | License | Text |
|---|---|---|
| Protocol definitions, client SDKs, recipe format, agent protocol | **Apache-2.0** | `LICENSE-APACHE` |
| Control plane, node agent, relay, web/chat/CLI clients | **AGPL-3.0-or-later** | `LICENSE-AGPL` |
| The documents in this repository, this one included | **CC BY 4.0** | `LICENSE-CC-BY` |
| Recipes, extensions, themes | Author's choice, including proprietary | |

The relay is named because it is a separate binary (§3.1 of `02-architecture.md`) and should not be
left for a reader to place.

### DCO, not a CLA

Contributions are accepted under a **Developer Certificate of Origin**. There is no contributor
licence agreement, no copyright assignment, and no entity holding the right to relicense the
project.

This is deliberate and it deliberately costs something. A CLA concentrates relicensing power in one
organization, and that concentration is the mechanism by which open projects are later taken
closed, a different route to the outcome MIT permitted for Pterodactyl, and one that has been
walked by several well-known projects since. Distributed copyright makes a future relicense require
broad consent from contributors.

**What the licence protects, and what is only policy.** Earlier versions of this section said the
licence structure "forecloses an open-core pivot" and that P10 and K-13 were "enforced by the
licence structure". That overstated it, and it is corrected here (2026-10-12):

- **Protected by the licence:** code released under AGPL-3.0-or-later stays available under it.
  Nobody, the maintainer included, can withdraw a released version, and nobody can take a
  contributor's code proprietary without that contributor's consent.
- **Not protected by the licence:** an open-core pivot does not need to relicense anything. It can
  ship new proprietary components beside the open core, and the extension model of §5 of
  `00-overview.md` explicitly permits proprietary extensions. And while one person has written all
  of the code, that person holds all of the copyright and could release future versions under other
  terms; distributed copyright constrains this only as other contributors' code accumulates.
- **What prevents it is policy:** P10 and K-13, written in this repository. They are promises, made
  in public and dated, and this document is honest that they are promises.
- **"Or later":** the `-or-later` clause lets each recipient choose a later version of the AGPL
  published by the Free Software Foundation. That is the FSF's power, not this project's, and the
  licence requires later versions to be "similar in spirit"; it cannot turn Korpis proprietary.

### Forks are welcome

Fork it, modify it, run it, sell hosting on it. **Running** Korpis, unmodified, to serve your own
customers obliges nothing. Two things do create AGPL obligations, and both are narrower than
commonly feared:

- **Modifying** Korpis and letting users interact with the modified version over a network (AGPL
  section 13): offer those users the source of your version.
- **Conveying** copies, which includes distributing binaries or the CLI, and the web client's
  JavaScript bundle that a browser downloads (sections 4 to 6): pass on the licence and make the
  corresponding source available. Every Korpis build states the exact source revision it was built
  from and where to get it, in `korpis version` (§8 of `13-surface-cli.md`) and at the web client's
  `/about/source` (§7 of `11-surface-web.md`), so an unmodified deployment meets this as shipped.

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

**Korpis has one maintainer.** That is stated first because §1's finding is that a project whose
velocity is one person's availability fails in a specific way, and a governance document that
described a two-maintainer rule the project could not keep would be the same failure written down.

> Corrected 2026-10-12. Earlier versions of this section required "two maintainers with
> independent, complete rights from the first commit" and forbade any single person from merging
> to a release branch. That described a project that does not exist. The rules below are the ones
> that can be kept, and they say what changes when a second maintainer exists.

**What substitutes for a second reviewer, while there is none.** Not a promise of care; mechanisms:

- Every change to a release branch passes the full conformance suites (§8) in CI, and a release is
  cut only from a green build.
- Builds are reproducible from the repository, and every release artifact is signed and recorded in
  a public transparency log, so anyone can check that a release is what the tagged source produces.
- Security fixes are published with the advisory detail of §8 and are open to retrospective review
  by anyone; a fix made in private under embargo is reviewed in public the day it is disclosed.
- Changes to the subjects listed in §7 still go through a written proposal and a comment period,
  which is the one form of review that one maintainer can always obtain.

**Infrastructure is held so that it outlives one person's account.** This is the real bus factor:
not who reviews, but who can log in.

| Asset | Requirement |
|---|---|
| domain registration | registered to the project, with a recovery contact recorded in the succession document |
| registry namespaces (recipes, extensions, images) | held by an organization account, never a personal one |
| release signing keys | an offline root key with an escrowed copy; online release keys are subkeys that the root can rotate |
| CI and build infrastructure | reproducible from configuration in the repository |
| package repositories | held by an organization account |
| the source repository organization | the maintainer plus a recovery owner who holds no commit rights |

**Succession is documented before it is needed.** A succession document in the repository names
who receives access if the maintainer is unreachable for a stated period, what they are entitled
to do with it (keep the lights on, publish security fixes, hand the project on), and what they are
not (change the licence policy, P10, or K-13). Until it exists, the project is not ready for a
first release, and §9 of `20-roadmap.md` lists it as a release blocker.

**How a second maintainer is added.** For sustained contribution and judgement, not for volume,
with the proposal process of §7. When there are two, the rules tighten automatically rather than by
a new decision: release signing becomes a two-of-*n* threshold, every release-branch change needs a
second maintainer's approval, and the recovery owner of the table above becomes a maintainer.

**No foundation initially.** Standing one up before there is anything to steward is expensive
ceremony. But the assets above are held so that transferring them to a neutral entity later is a
transfer rather than a negotiation.

---

## 7. Decisions, and the cost of silence

Changes to **the domain model, the protocol, the security boundary, the licence, or the governance
rules** require a written proposal, a comment period, and an explicit decision. Everything else
runs on lazy consensus.

The rule that matters most is about the decisions that are never made:

> **Silence is not a decision, and an untriaged issue is a process failure rather than a backlog
> item.** Every issue receives a triage response within a published window. "We are not going to do
> this, because, " is a good outcome. A batch closure with no reason given is not.

**Closing as *not planned* requires a stated reason, written on the issue.** Closing a
*correctness or security* report as not planned also requires the rationale to be recorded in the
next release notes, where users of the affected behaviour will see it, and, once there are two
maintainers, a second maintainer's agreement. The 858 GB-over-quota report (#4554) is the reason:
it had been explained in a comment two years earlier, and was then closed as not planned without a
word, in a batch of 86 issues closed on one day in May 2024. It is the clearest example in the
research of a process producing an outcome nobody would have chosen deliberately.

> Corrected 2026-10-12. This section used #141 as its example of an issue harmed by never being
> answered. #141 was answered the same day (§1.6 of `research/evidence.md`).

The roadmap is public, dated, and says what is *not* being done, §2 of `00-overview.md` is the
permanent version of that, and per-release scope is the temporary one.

---

## 8. Contribution

The conformance suites are the contribution gate and the reason this scales past one maintainer. A
runtime driver is accepted if it passes the driver suite (§8 of `04-runtimes.md`); an agent
implementation is conforming if it passes the agent suite (§11 of `10-api.md`). Neither requires a
maintainer to hold the whole system in their head to review, which is the bottleneck that throttles
projects of this size.

Documentation lives in the repository and changes with the code. A behaviour change that does not
update the document describing it is incomplete, and the twenty-four documents in this directory
are the specification, not a description written afterwards.

Security disclosure has a published contact and a stated response window (§12 of `17-security.md`).
Advisories are published with the same detail Korpis' own research demanded of others: what was
reachable, what it affected, which versions, and what the fix changed.

---

## 9. Money, stated honestly

This is the largest unresolved risk in the project and it is not a technical one.

P10 forbids taxing growth and K-13 rules out open-core as policy (§2 says what the licence adds
and what it does not), which together remove every revenue
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
telemetry backend, no hosted service), and the succession document of §6 is what lets the project
outlive one person's economics changing. With one maintainer that is the weakest point in this
section, and it is stated rather than hidden.

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
6. **Dormancy.** §6 covers a maintainer who is unreachable. A maintainer who is reachable and has
   stopped is harder: what counts as dormant (no release, no triage, for how long), and whether the
   trademark policy of §3 then lets a successor fork use the name, is unwritten. → here
