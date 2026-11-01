# Governance

How Korpis is licensed, named, maintained and decided. Versioning, releases and deprecation are in
[docs/operations/release.md](docs/operations/release.md); contributing is in
[CONTRIBUTING.md](CONTRIBUTING.md), and disclosure in [SECURITY.md](SECURITY.md).

Pterodactyl's architectural problems were survivable; its governance problems were not. A rewrite
was announced and abandoned, the control-plane and daemon contract was whatever the reference
implementation did ([pterodactyl/panel#141](https://github.com/pterodactyl/panel/issues/141)),
correctness reports were closed in bulk without a stated reason ([pterodactyl/panel#4554](https://github.com/pterodactyl/panel/issues/4554), a file 858 times larger than
its limit, among 86 issues closed on one day in May 2024), and the project's velocity was one
person's availability until the community forked. Each is a process outcome, so licence,
compatibility and maintainership are settled before the first release rather than when contested.

## Licence

| Layer | Licence | Text |
|---|---|---|
| Protocol definitions, client SDKs, the recipe format, the agent protocol, the conformance suites | Apache-2.0 | `LICENSE-APACHE` |
| Control plane, node agent, relay, panel, chat and CLI clients | AGPL-3.0-or-later | `LICENSE-AGPL` |
| Documentation | CC BY 4.0 | `LICENSE-CC-BY` |
| Recipes, extensions, themes | the author's choice, proprietary included | |

A third party runs a conformance suite against what it wrote, so the suites are as free to use as the
protocols they test. Contributions come under the Developer Certificate of Origin (`git commit -s`).
There is no contributor licence agreement, no copyright assignment and no entity holding the right to
relicense: a CLA concentrates relicensing power in one organization, which is how open projects are
later taken closed. Distributed copyright makes a relicense require broad consent.

**What the licence protects, and what is only policy:**

- Protected: code released under AGPL-3.0-or-later stays available under it. Nobody, the maintainer
  included, can withdraw a released version or take a contributor's code proprietary without their
  consent.
- Not protected: an open-core pivot needs no relicense; it ships proprietary components beside the
  open core, which the extension model permits. While one person has written all the code, that person
  holds all the copyright and could release future versions on other terms; distributed copyright
  constrains this only as other contributors' code accumulates.
- What prevents it is policy: "no tax on growth" and "no feature gated by edition" in
  [AGENTS.md](AGENTS.md) are public promises, and this page is honest that they are promises.
- "Or later" lets a recipient choose a later AGPL from the Free Software Foundation, which must be
  similar in spirit; it cannot make Korpis proprietary.

**Forks are welcome.** Running Korpis unmodified to serve customers obliges nothing. Modifying it and
letting users interact with it over a network (AGPL section 13) obliges offering them the source.
Conveying copies, the CLI and binaries and the panel's JavaScript a browser downloads (sections 4 to
6), obliges passing on the licence and the corresponding source. Every build names its exact source
revision and where it is published, in `korpis version` and at the panel's `/about/source`, so an
unmodified deployment meets this as shipped. Hosts are never blocked, restricted, capped or charged:
no per-workload, per-node, per-instance, per-account or per-domain anything, at any scale.

## Trademark

The name and marks are held separately from the code, which is what makes the licence's permissiveness
safe. Unmodified builds may use them freely, and commercial hosting under the name ("we run Korpis",
"Korpis hosting") needs no permission and owes nothing. A modified distribution is renamed, not because
modification is discouraged but because a user who downloads something called Korpis must get Korpis
and an advisory must refer to a knowable artifact. The marks are never used to restrict use,
competition or resale.

## Maintainership

Korpis has one maintainer. A rule requiring two that the project cannot keep would be the failure
above written down, so what substitutes for a second reviewer is mechanism, not a promise of care:

- Every change to `main` arrives through a pull request whose checks pass in CI, the conformance
  suites included, and a release is cut only from a green build.
- Builds are reproducible from the repository, and every release artifact is signed and recorded in a
  public transparency log, so anyone can check a release is what its tag produces.
- A security fix made under embargo is reviewed in public the day it is disclosed.
- Changes to the subjects below go through a written proposal and a comment period, the one review one
  maintainer can always obtain.

**Infrastructure outlives one account.** The real bus factor is who can log in:

| Asset | Held |
|---|---|
| domain | registered to the project, with a recovery contact named in the succession document |
| registry namespaces (recipes, extensions, images) and package repositories | organization accounts, never personal ones |
| release signing keys | an offline root with an escrowed copy; online release keys are subkeys the root rotates |
| CI and build infrastructure | reproducible from configuration in the repository |
| the source organization | the maintainer and a recovery owner who holds no commit rights |

**Succession is written before it is needed**: who receives access if the maintainer is unreachable
for a stated period, what they may do (keep the lights on, publish security fixes, hand the project
on) and may not (change the licence policy or the promises above). The first release waits on it.

**A second maintainer** is added for sustained contribution and judgement, not volume, through the
proposal process. With two, the rules tighten without a new decision: release signing becomes a
threshold of two, every change to `main` needs the other maintainer's approval, and the recovery owner
becomes a maintainer. No foundation is formed before there is something to steward, but the assets
are held so that moving them to one later is a transfer, not a negotiation.

## Decisions

Changes to the domain model, the protocols, the security boundary, the licence or this page need a
written proposal (an issue labelled `proposal`), a comment period and an explicit decision on the
issue. Everything else runs on lazy consensus through pull requests.

**Silence is not a decision.** Every issue receives a triage response within a published window.
"We are not going to do this, because ..." is a good outcome; a batch closure with no reason is not.
Closing as not planned states the reason on the issue. Closing a correctness or security report as
not planned also records the reason in the next release notes, where users of the behaviour see it,
and, with two maintainers, needs the other's agreement. The roadmap is public as milestones, and says
what is not being done.

Removing a design decision takes the same argument it took to add it, in its own pull request.

## Contribution scales through conformance

A runtime driver is accepted when it passes the driver suite
([driver conformance](docs/internals/driver-conformance.md)), and an agent implementation conforms
when it passes the agent suite. Neither needs a maintainer to hold the whole system in their head,
which is the bottleneck that throttles projects of this size. Documentation lives in the repository
and changes with the code in the same pull request.

## Money

The largest unresolved risk is not technical. The promises above remove every revenue mechanism the
commercial products in this market use: per-server, per-machine, per-instance, per-account and
per-domain licensing, and reselling limits below a top tier. That is the point: Pterodactyl took the
market by removing the tax, and imposing one forfeits a new entrant's only durable advantage. What
remains is donations and sponsorship; support, consulting and managed hosting offered by anyone,
maintainers included, as separate businesses; and funded development of specific features delivered
into the open project. Several projects have died in this position. The mitigations are structural:
Korpis operates no infrastructure (no marketplace, registry, telemetry backend or hosted service), so
its costs stay low, and succession lets the project outlive one person's economics changing.

## Not yet decided

Each is an open `question` issue: what triggers a foundation; who adds to the first-party recipe set
beyond its maintained recipes (a first-party recipe has a named maintainer, and one whose maintainer
steps away is relabelled community); the boundary on funded development that conflicts with the
principles; who approves a translation into a language no maintainer reads; the line between an LTS
correctness fix and a behaviour change; and what counts as a dormant maintainer and whether a
successor fork may then use the name.
