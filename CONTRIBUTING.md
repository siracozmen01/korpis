# Contributing

Korpis is built in the open by one maintainer, and disagreement is as welcome as code. A design nobody
has argued with has not been reviewed, and this one departs deliberately from how this class of
software is usually built; each departure is a place it could be wrong.

## What helps most

- **Find a hole in the reasoning.** If a page in [docs/internals](docs/internals/) asserts something
  it has not earned, or leans on an assumption it never states, open an issue quoting it.
- **Attack the definitions.** Take one page and ask of every field, condition and limit: what
  evaluates this, where, and with what information? That reading has found more defects than any
  other, including a limit that was displayed and could not be enforced.
- **Correct a claim about another system.** Pages cite other projects' issues and advisories. If one
  is wrong, out of date or unfair, say so; if you maintain that project, your correction takes
  precedence over our reading.
- **Bring operational experience.** If you run this kind of infrastructure at scale, you know which of
  these decisions will hurt at 3am, which no design page can supply.
- **Take an open question.** Undecided design questions are issues labelled `question`.

## What to expect

Every issue gets a response; "we are not going to do this, because ..." is a fine outcome, silence is
not. Changes to the domain model, the protocols, the security boundary, the licence or the governance
need a written proposal and an explicit decision; everything else runs on lazy consensus
([GOVERNANCE.md](GOVERNANCE.md)).

## Making a change

1. Find or open the issue. Plans and progress go in it, never in a file.
2. Branch from `main`, named for the issue, or for the batch where several issues of a milestone
   touch the same code and land together.
3. Build and test as [development](docs/operations/development.md) describes, and run
   `scripts/check.sh` before pushing and `scripts/check.sh pr` before opening the pull request
   ([checks](docs/operations/checks.md)).
4. Open one pull request that closes the issue, or each issue of the batch. Its title is a
   conventional commit, `type(scope): summary`, lower case, under 72 characters. Its body has
   Problem, Change and Verification (the commands you ran and what they showed) and ends with a
   `Closes #<issue>` line for each issue it closes.

A change to behaviour rewrites the page in `docs/internals/` that describes it, in the same pull
request. [AGENTS.md](AGENTS.md) holds the rules that are easy to get wrong, written for AI coding agents
and holding for people too.

## Legal

Contributions are accepted under the **Developer Certificate of Origin**. Sign off your commits:

```
git commit -s
```

There is no contributor licence agreement and no copyright assignment, so relicensing anyone's
contribution needs that person's consent. That alone does not make an open-core turn impossible;
[GOVERNANCE.md](GOVERNANCE.md) says which promises the licence enforces and which are policy. A
contribution is licensed like the layer it lands in: documentation under CC BY 4.0; protocol
definitions, SDKs and conformance suites under Apache-2.0; the platform under AGPL-3.0-or-later. See
[LICENSE](LICENSE).
