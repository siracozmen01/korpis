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
[`21-stack.md`](./docs/21-stack.md)). How it is organized, and why, is in
[`ARCHITECTURE.md`](./ARCHITECTURE.md): read it before adding a file. Five scripts cover
everything:

```
scripts/dev-pg.sh up      a PostgreSQL for development and tests, in its own container on
                          127.0.0.1:55432, isolated from anything else on the machine
scripts/dev-registry.sh up
                          an OCI registry for recipes, in its own container on
                          127.0.0.1:55000, plain HTTP on loopback only; the images the
                          tests run are mirrored into it by digest the first time, so no
                          test pulls from a public registry
scripts/tools.sh          the pinned tools (buf, the code generators, sqlx-cli, oras), into
                          .tools/; needed to change a .proto (then scripts/gen-proto.sh) or a
                          query, and to run the end-to-end script
scripts/test-tools.sh     the servers some tests run against (MinIO, built from its pinned release
                          with a pinned Go), into .tools/; a test whose server is missing says
                          so and does not run
scripts/sqlx-prepare.sh   after changing a migration or a query: rebuilds the development
                          database and regenerates .sqlx/, which builds use offline
scripts/check.sh          every check a commit must pass: the text rules, file length, the
                          layer rule, the schema, rustfmt, clippy with warnings denied, the
                          tests, and the dependency policy
scripts/e2e-phase0.sh     the Phase 0 done-when (§4 of docs/20-roadmap.md) as an operator runs
                          it: the five binaries as processes, the recipe published with oras,
                          every step through the CLI; needs root and containerd, and leaves
                          nothing behind
```

Building needs only a Rust toolchain: the wire types are generated at build time from the
committed descriptor set, and queries are checked against the committed `.sqlx/` metadata, so
neither a protobuf compiler nor a database is needed to compile.

## Code rules

These are the rules professional Rust teams converge on, written down so that they hold in a
codebase with one maintainer as well as in one with fifty. Most are enforced by
`scripts/check.sh`; the rest are enforced in review, and a change that breaks one is wrong in the
same way a document with an em dash is.

**Layers.** The dependency rule of §3 of [`ARCHITECTURE.md`](./ARCHITECTURE.md) is absolute:
the domain does no I/O, the application never names a database or a transport, and a handler holds
no business rule. `scripts/check-layers.sh` fails the build on an outward dependency.

**Size.** No source file exceeds 400 lines, tests included (checked). No function exceeds 60 lines
(clippy `too_many_lines`), takes more than 6 arguments (`too_many_arguments`), or nests more than
4 blocks deep (`excessive_nesting`). A file near the limit is split by responsibility, never by
cutting at line 400. Long SQL lives in `crates/korpis-store/queries/` and is loaded with
`query_file!`.

**Modules.** A module with children is `name.rs` beside `name/`, never `name/mod.rs`
(`mod_module_files`). Code is grouped by bounded context (§4 of `ARCHITECTURE.md`), and a context
has the same module name in every layer.

**No panics outside tests.** `unwrap`, `expect`, `panic!`, indexing that can panic, `todo!` and
`unimplemented!` are denied in non-test code. A function that can fail returns a `Result`; an
invariant the type system cannot express is made expressible, or handled. Something specified but
not yet built answers with a typed "not yet, Phase N" error, never with a panic.

**Errors.** Libraries define their errors with `thiserror`, one enum per layer, and translate at
each boundary: the store's errors into `StoreError`, use-case refusals into `AppError`, and those
into the wire's `korpis.v1.Error`. No `Box<dyn Error>` crosses a library boundary; `main` may use
one. A caller is never told more than §8 of [`10-api.md`](./docs/10-api.md) allows.

**Lints.** `clippy::all` is denied and `clippy::pedantic` is warned, and warnings fail the build.
An exception is `#[expect(lint, reason = "...")]` on the narrowest item, never a bare
`#[allow]` (`allow_attributes_without_reason`), and the reason says why the code is right, not
that the lint is annoying. Integration test crates start with one `#![allow]` block for the
panicking lints, with a reason.

**Documentation.** Every public item of `korpis-core`, `korpis-app`, `korpis-store` and
`korpis-server` has a doc comment (`missing_docs`). Comments say why, cite the document and
section that decided it, and are written in the same English as the specification.

**Output.** Libraries log through `tracing` and never print (`print_stdout`, `print_stderr`); a
binary prints only in the functions that produce its output, marked with `#[expect]`.

**Unsafe.** `unsafe_code` is denied workspace-wide. The day a crate needs it, the crate says so in
its header, and every block carries a `// SAFETY:` comment that a reviewer can check.

**Tests.** Unit tests sit at the foot of the file they test; tests that need a database or cross
a crate boundary live in the crate's `tests/` directory, one file per behaviour, with shared
setup in a `support` module. A test is named for the behaviour it pins down
(`a_link_form_grant_can_never_delegate`), not for the function it calls.

**Formatting.** `rustfmt` with `rustfmt.toml`, unconditionally. Lines of prose in comments stay
under 100 characters, like the documents.

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
