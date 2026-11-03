# Development

How to build Korpis, run what its tests need, and write code that passes review. The checks are in
[checks](checks.md), and where a new piece of code goes is in
[architecture](../internals/architecture.md).

## Contents

- [Building](#building)
- [Services and tools](#services-and-tools)
- [Generated files](#generated-files)
- [End-to-end scripts](#end-to-end-scripts)
- [Code rules](#code-rules)
- [Tests](#tests)
- [Documentation](#documentation)
- [Work](#work)

## Building

Building needs a Rust toolchain with the musl target `rust-toolchain.toml` names, which
`korpis-guest` is built static for. The wire types are generated at build time from the committed
descriptor sets, and queries are checked against the committed `.sqlx/` metadata, so neither a
protobuf compiler nor a database is needed to compile. `.env` points `DATABASE_URL` at the
development database: set `SQLX_OFFLINE=true` for any `cargo` command outside `scripts/check.sh`, or
the query macros check against whatever that database holds.

The panel in `web/` needs Node 22 or later and pnpm, which corepack fetches at the exact version and
digest `web/package.json` names. Its generated API client is committed in `web/src/gen`, so building
it needs no protobuf tool either.

## Services and tools

| Script | What it gives |
|---|---|
| `scripts/dev-pg.sh up` | PostgreSQL for development and tests, container `korpis-dev-postgres` on 127.0.0.1:55432, in memory and never synced: `down` loses its data |
| `scripts/dev-registry.sh up` | an OCI registry on 127.0.0.1:55000, plain HTTP on loopback; the images tests run are mirrored into it by digest once, so no test pulls from a public registry |
| `scripts/tools.sh` | the pinned tools in `.tools/`: buf, the code generators, sqlx-cli, oras, Firecracker and its jailer |
| `scripts/guest-kernel.sh` | the kernel every microVM boots, built from kernel.org's pinned release and `guest/kernel/` into `.tools/guest/` |
| `scripts/test-tools.sh` | the servers and browsers some tests run against (MinIO built from its pinned release, the OIDC issuer, Playwright's Chromium) |

Tests find the database and registry through `KORPIS_TEST_PG_URL` and `KORPIS_TEST_REGISTRY`,
defaulting to the two containers. A test needing root, KVM, containerd or one of these tools says so
in one line and does not run without it; present and failing, it fails.

## Generated files

Regenerate, never edit:

- after changing a `.proto`, comments included: `scripts/gen-proto.sh && (cd web && pnpm generate)`,
  which rewrites `crates/korpis-proto/descriptor.binpb`, `crates/korpis-containerd/descriptor.binpb`
  and `web/src/gen/`;
- after changing a migration or a query: `scripts/sqlx-prepare.sh`, which rebuilds the development
  database and rewrites `.sqlx/`.

A stale generated file fails the check. Every migration after 0074 is an `.up.sql` with a
`.down.sql` whose inverse is tested, and a released migration is never edited.

## End-to-end scripts

`scripts/e2e-phase0.sh` to `e2e-phase3.sh` run each phase's completion scenario as an operator
would: the binaries as processes, recipes published with oras, every step through the CLI. Phase 1
adds an extension installed from its static image, a console read and typed into, a volume quota
refused by the kernel, and a backup taken, read and restored into a clone; phase 2 a reseller, its
sub-tenant and their customer, each acting with a credential the level above issued, and a person
signed in through `korpis-idp-oidc` against the independent issuer; phase 3 two nodes, each with its
data plane in a network namespace standing in for its machine, meshed over a veth, and a workload
with a 40 GB volume filled with 32 GiB moved between them, reached at the same address by a client
network that follows the test extension's address router. They need root and containerd (and
btrfs-progs, the kernel's WireGuard, Node.js and the issuer's packages where they use them), share
`scripts/e2e-lib.sh`, which enrolls one machine as one node or several with their own ports, pools,
namespaces, volumes and cgroups, and leave nothing behind. `scripts/check.sh` runs none of them.

`scripts/test-install-package.sh` runs `korpis-server install --postgres package` in systemd
containers of Debian, Ubuntu and Fedora, and needs Docker and the network.

## Code rules

Most are enforced by `scripts/check.sh`; the rest in review.

- **Layers.** The domain (`korpis-core`) does no I/O, the application (`korpis-app`) never names a
  database or a transport, and a handler holds no business rule. `scripts/check-layers.sh` fails on
  an outward dependency.
- **Size.** No source file over 400 lines, tests included. No function over 60 lines
  (`too_many_lines`), over 6 arguments (`too_many_arguments`) or nested over 4 blocks
  (`excessive_nesting`). A file near the limit is split by responsibility. Long SQL lives in
  `crates/korpis-store/queries/` and is loaded with `query_file!`.
- **Modules.** A module with children is `name.rs` beside `name/`, never `name/mod.rs`. Code is
  grouped by bounded context, with the same module name in every layer.
- **No panics outside tests.** `unwrap`, `expect`, `panic!`, panicking indexing, `todo!` and
  `unimplemented!` are denied in non-test code. Something designed but not built answers with a
  typed "not yet" error.
- **Errors.** `thiserror`, one enum per layer, translated at each boundary: the store's into
  `StoreError`, refusals into `AppError`, those into the wire's `korpis.v1.Error`. No `Box<dyn
  Error>` crosses a library boundary. A caller is never told more than [api](../internals/api.md)
  allows.
- **Lints.** `clippy::all` denied, `clippy::pedantic` warned, warnings fail. An exception is
  `#[expect(lint, reason = "...")]` on the narrowest item, never a bare `#[allow]`, and the reason
  says why the code is right.
- **Comments** say why, never what, and never cite a section number or a record. Every public item
  of `korpis-core`, `korpis-app`, `korpis-store` and `korpis-server` has a doc comment.
- **Output.** Libraries log through `tracing` and never print; a binary prints only in the functions
  that produce its output.
- **Unsafe** is denied workspace-wide; a crate that needs it says why in its header, and every block
  carries a `// SAFETY:` comment.
- **Licences.** Every source file starts with its SPDX line: `Apache-2.0` in `korpis-proto`,
  `korpis-containerd`, `korpis-extension`, `korpis-driver-conformance`, `korpis-conformance-probe`
  and `web/src/gen`, `AGPL-3.0-or-later` elsewhere.
- **Formatting.** `rustfmt` with `rustfmt.toml`, unconditionally.

## Tests

Tests pin behaviour through the public interface: wire and storage formats, hostile input, grants
and isolation, state machines and reconciliation, and every bug class that escaped once.

- A bug fix comes with the fewest tests that fail on the unfixed code for the right reason, in the
  module's existing test file. A feature tests what a caller relies on, not every input; a case is
  added only where the code branches differently.
- No test of what the type system guarantees, of call order or private state, or of mocks wired to
  mirror the code. A visual change is proven by a capture, not a test.
- Database tests run against real PostgreSQL. Each gets a fresh database cloned from a migrated
  template, so no test pays for the migrations.
- Tests are deterministic: no sleeps, wall clock, polling loops or retries. A test waits on the
  event or state marking the milestone, and a flaky test is fixed, not retried.
- Unit tests sit at the foot of the file they test. Integration tests live in `tests/`, one file per
  behaviour, named for it (`a_suspended_reseller_suspends_its_customers_grants_and_not_the_hosts`).

## Documentation

`docs/internals/` is the design the code is held to: one living page per topic, in the present
tense, giving each decision with its reasoning, its cost and what it does not do. When a decision
changes, the page is rewritten in the same pull request, with no amendments, dates or record
numbers; history is in git. `docs/user/` follows the reader's task, and `docs/operations/` holds
this page, the checks and releases. Pages stay under 400 lines, with contents over 100. A claim
about another system cites its issue, advisory or documented behaviour inline. Every field,
condition or limit a page adds says what evaluates it, where, and with what information. A rule
stated on several pages is changed on all of them. Everything is English, and no file contains the
em dash character. `README.tr.md` translates `README.md`, and a change to one updates the other.

## Work

Work is tracked in GitHub issues grouped by milestone; plans and progress go in the issue, never in
a file. One issue, one branch named for it, one pull request closing it. A pull request title is a
conventional commit (`type(scope): summary`, lower case, under 72 characters); its body has Problem,
Change and Verification (the commands run and what they showed) and ends with `Closes #<issue>`.
Commits are signed off (`git commit -s`). Pull requests are squash-merged, and `main` changes only
through them.
