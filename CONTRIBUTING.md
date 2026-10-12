# Contributing

The code has only just started (Phase 0, in `crates/`). **The most valuable contribution right now
is still disagreement.**

A specification nobody has argued with has not been reviewed, and this one makes a number of
deliberate departures from how this class of software is normally built. Each of those departures
is a place it could be wrong.

## What helps most

**Find a hole in the reasoning.** If a document asserts something it hasn't earned, or leans on an
assumption it never states, that is worth an issue. Quote the section.

**Find a failure mode the walkthroughs missed.** [`23-walkthroughs.md`](./docs/23-walkthroughs.md)
traces fourteen scenarios end to end specifically to find seams between documents. It found
twenty-three. There are almost certainly more, and the fifteenth scenario nobody has traced is
where they are.

**Attack the definitions, not just the seams.** The other productive reading is to take a single
document and ask of every field, condition, and limit in it: what evaluates this, where, and with
what information? §16 of [`23-walkthroughs.md`](./docs/23-walkthroughs.md) is a review done that
way and it found seventeen things fourteen traces had walked straight past, including a limit that
was displayed and could not be enforced. §17 records a second review that read every document
against every other and checked every external claim against its primary source; it found
seventy-nine.

**Correct the evidence.** [`docs/research/evidence.md`](./docs/research/evidence.md) makes factual
claims about deployed software. If one is wrong, out of date, or unfair, please say so. Accuracy
matters more there than being right, and if you maintain one of the projects discussed, your
correction takes precedence over our reading.

**Bring operational experience.** If you run this kind of infrastructure at scale, the thing you
know that no design document can supply is which of these decisions will hurt at 3am. That is the
review this most needs.

**Take an open question.** Fifty-nine are open across the documents, each filed where it will be
settled. Several are genuine forks in the road rather than details, and are marked as such.

## What to expect

Every issue gets a response. §7 of [`19-governance.md`](./docs/19-governance.md) commits to that,
and to the reason: an untriaged issue is a process failure, not a backlog item. "We are not going
to do this, because X" is a fine outcome. Silence is not.

Changes to the domain model, the protocol, the security boundary, the licence, or the governance
rules go through a written proposal and an explicit decision. Everything else runs on lazy
consensus.

## Style

Documents are the specification, not a description of code written elsewhere. They are meant to be
read start to finish by someone who has not seen them before, so:

- State the decision, then the reasoning, then what it costs. Never only the first.
- Cite the evidence when a claim rests on one. "Because X happened, here, with this issue number"
  is worth more than a confident sentence.
- Say what a thing does *not* do. Most of the value in the security and operations documents is in
  the honest negatives.
- Record corrections in place rather than rewriting history. §2 of
  [`01-model.md`](./docs/01-model.md) is the model for this.
- Keep lines under 100 characters, and prefer plain prose to bullet fragments.
- **Never use the em dash character.** Restructure the sentence: a comma for an aside, a colon
  before an explanation, parentheses for a parenthetical, a semicolon or a full stop between two
  independent clauses. A spaced hyphen is not a substitute. This is a hard rule for the project and
  it is checked before every commit.

[`CLAUDE.md`](./CLAUDE.md) holds the full set of conventions, including which document owns which
decision and how open questions are recorded. It is written for AI coding agents, and it is the
shortest accurate description of how this repository works for humans too.

## Working on the code

The implementation is a Rust workspace in `crates/` (§3 of
[`21-stack.md`](./docs/21-stack.md)). Three scripts cover everything:

```
scripts/dev-pg.sh up    a PostgreSQL for development and tests, in its own container on
                        127.0.0.1:55432, isolated from anything else on the machine
scripts/tools.sh        the pinned schema tools (buf and the code generators), into .tools/;
                        needed only to change a .proto, then run scripts/gen-proto.sh
scripts/check.sh        every check a commit must pass: the text rules, the schema, rustfmt,
                        clippy with warnings denied, the tests, and the dependency policy
```

Building needs only a Rust toolchain: the wire types are generated at build time from the
committed descriptor set, so no protobuf compiler is required.

## Legal

Contributions are accepted under the **Developer Certificate of Origin**. Sign off your commits:

```
git commit -s
```

There is no contributor licence agreement and no copyright assignment. This is deliberate. It keeps
copyright distributed, so that relicensing anyone's contribution needs that person's consent. It
does not, on its own, make Korpis impossible to take open-core; §2 of
[`19-governance.md`](./docs/19-governance.md) says plainly which promises the licence enforces and
which are policy.

A contribution is licensed like the layer it lands in: documents under CC BY 4.0, protocol
definitions and SDKs under Apache-2.0, and the platform under AGPL-3.0-or-later. See
[`LICENSE`](./LICENSE).

Licensing is split by layer: protocol and SDK material is Apache-2.0, the platform is AGPL-3.0. The
document you are editing tells you which side of that line it describes; when in doubt, ask in the
issue first.
