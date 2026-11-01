# Korpis

Korpis is an open-source platform for running workloads (game servers, bots, web services,
databases, jobs, virtual machines) on machines their operators control. A control plane
(`korpis-server`, PostgreSQL) holds the declared state; agents on the nodes converge reality toward
it and report what is observed; the web panel, the CLI, chat and the API are peer clients of one
API. It is a Rust workspace in `crates/` and a TypeScript panel in `web/`.
[docs/00-overview.md](docs/00-overview.md) explains what it is for and how the parts fit.

It never compromises on:

1. **Declared truth.** Clients declare intent; reconcilers converge toward it. No component
   commands another and assumes it happened. Every status shown is observed, or says unknown.
2. **The kernel confines.** Privilege is split by process and confined by namespaces, cgroups,
   capabilities, seccomp, Landlock and `openat2(RESOLVE_BENEATH)`. Checking a path or a name as a
   string is defense in depth, never the defense.
3. **No limit without enforcement.** A limit that is displayed is enforced by the kernel and
   metered, or it is not displayed.
4. **A plan before every effect.** Every change produces a persisted, inspectable plan first.
   Every operation that changes durable state is resumable, verifiable and reversible.
5. **Grants, not roles.** Authority is one primitive, a grant, that only ever yields weaker
   children.
6. **No tax on growth.** No per-workload, per-node, per-instance, per-account or per-domain limit,
   charge or gate, at any scale, and no feature gated by edition.
7. **Extensions are peers.** First-party features use the same extension contract third parties
   do. If the contract is not enough for core, the contract is fixed.
8. **Scope is not reduced quietly.** A design decision is removed only with the same argument it
   took to add it, in its own pull request.

## Commands

```sh
scripts/dev-pg.sh up                 # PostgreSQL for tests, 127.0.0.1:55432 (korpis-dev-postgres)
scripts/dev-registry.sh up           # OCI registry for tests, 127.0.0.1:55000
cargo test -p korpis-core --features authz --test authz a_chain_is_in_force_only_while_every_grant_in_it_is
cargo clippy -p <crate> --all-targets -- -D warnings                         # one crate
scripts/check-web.sh                 # the panel: lint, types, unit tests, build
/home/rawframe-project/tools/heavy.sh scripts/check.sh   # every check; always through heavy.sh
scripts/sqlx-prepare.sh              # after changing a migration or a query
scripts/gen-proto.sh && (cd web && pnpm generate)        # after changing any .proto, comments too
```

Builds read `.env`, whose `DATABASE_URL` points at the development database: set
`SQLX_OFFLINE=true` for any `cargo` command outside `check.sh`, or the query macros check against
whatever that database holds. Tests that need the database or the registry find them through
`KORPIS_TEST_PG_URL` and `KORPIS_TEST_REGISTRY`, defaulting to the two containers above.
`scripts/e2e-phase{0,1,2}.sh` run the phase end-to-end scenarios as an operator would; they need
root and containerd and leave nothing behind.

## Ways to hurt yourself

The development machine is shared with other projects, their CI runners and their sessions.

- **Killing by pattern.** Never `pkill -f`, `pgrep | kill` or `kill` a PID found by matching a
  name or path. Kill only a PID you started.
- **Other projects' containers and namespaces.** Use only the containerd namespace `korpis`, the
  two development containers above, and loopback ports. Never touch Docker's `moby` namespace,
  other containers, the system PostgreSQL, the firewall chains, sysctls or kernel modules, and
  never restart containerd. `scripts/test-install-package.sh` needs Docker: it does not run here.
- **Starving the machine.** Full checks, the end-to-end scripts and large builds go through
  `/home/rawframe-project/tools/heavy.sh`, one at a time per session. Cap parallelism at 4
  (`CARGO_BUILD_JOBS=4`). Do not build in a checkout while a check runs in it.
- **Leaked test state.** End-to-end tests create network namespaces, cgroups, btrfs loop volumes
  and containerd tasks; stop what a run left behind, by the identifiers that run printed.
- **The host's GPU** is never passed through to a test workload.
- **Pushing to main.** Changes reach `main` only through a pull request.

## Invariants

- **Layers.** The domain (`korpis-core`) does no I/O; the application (`korpis-app`) never names a
  database or a transport; a handler holds no business rule. `scripts/check-layers.sh` enforces
  the direction. Where a new thing goes is in [ARCHITECTURE.md](ARCHITECTURE.md).
- **Generated files.** `crates/korpis-proto/descriptor.binpb`,
  `crates/korpis-containerd/descriptor.binpb` and `web/src/gen/` come from the `.proto` files;
  `.sqlx/` comes from the queries and migrations. Regenerate them, never edit them; a stale one
  fails the check.
- **Migrations.** Every migration after 0074 is an `.up.sql` with a `.down.sql` whose inverse is
  tested. A released migration is never edited.
- **Licences.** Every source file starts with its SPDX line: `Apache-2.0` in `korpis-proto`,
  `korpis-containerd`, `korpis-extension`, `korpis-driver-conformance`,
  `korpis-conformance-probe` and `web/src/gen`, `AGPL-3.0-or-later` everywhere else.
- **No panics outside tests.** `unwrap`, `expect`, `panic!`, panicking indexing and `todo!` are
  denied in non-test code. Something designed but not built answers with a typed "not yet" error.
- **Unsafe** is denied workspace-wide; a crate that needs it says why in its header, and every
  block carries a `// SAFETY:` comment.
- **Lint exceptions** are `#[expect(lint, reason = "...")]` on the narrowest item, never a bare
  `#[allow]`.
- **Size.** Source files stay under 400 lines and functions under 60, both checked; a file near
  the limit is split along a responsibility.
- **The name.** Korpis's package format is a recipe. Never call it, or anything, "Blueprint".
- **Wire contracts** (`korpis.v1`, the agent protocol, the extension SDK) change compatibly once
  released; before the first release they change on design merit alone.

## Testing

- Test behavior through the public interface: wire and storage formats, hostile input, grants
  and isolation, state machines and reconciliation, and every bug class that escaped once.
- A bug fix comes with the fewest tests that fail on the unfixed code for the right reason, in the
  existing test file of that module. A new feature tests what a caller relies on, not every
  input; add a case only where the code branches differently.
- No test of what the type system already guarantees, of call order, private state or mocks
  wired to mirror the code, and no test for a visual change (the proof is a capture).
- Database tests run against real PostgreSQL, never a mock of the store.
- Tests are deterministic: no sleeps, wall clock, polling loops or retries. Wait on the event or
  state that marks the milestone. A flaky test is fixed, not retried.
- A test that needs root, KVM, containerd or a tool from `scripts/test-tools.sh` says so in one
  line and does not run without it; present and failing, it fails.
- Integration tests in `tests/`, one file per behavior, named for the behavior
  (`a_suspended_reseller_suspends_its_customers_grants_and_not_the_hosts`); unit tests at the foot of the file they test.

## Documentation

- `docs/internals/` is the design the code is held to: one living page per topic, present tense,
  third person, giving the decision, its reasoning and its cost, and what it does not do. When a
  decision changes, the page is rewritten in the same pull request; no amendments, dates or
  record numbers. History lives in git and the pull requests.
- `docs/user/` follows the reader's task; `docs/operations/` holds development, checks, release.
- A change that touches a rule stated on several pages updates every page that states it.
- A claim about another system cites its issue, advisory or documented behavior inline.
- Every field, condition or limit a page adds answers: what evaluates this, where, and with what
  information? The answer names a component that holds that information.
- `README.tr.md` translates `README.md`; a change to one updates the other in the same pull
  request.
- Code comments say why, never what, and never cite a section number or a record.
- Pages stay under 400 lines. Everything in the repository is English, and no file contains the
  em dash character.

## Work

Work is tracked in GitHub issues, grouped by milestone. A session takes one issue, works on a
branch named for it, and opens one pull request that closes it. Plans, notes and progress go in
the issue, never in a file in the repository. A session does its work itself: subagents are
denied in `.claude/settings.json`.

## Pull requests

Titles are conventional commits, lower case, under 72 characters: `type(scope): summary`. Types:
`feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`, `ci`, `chore`. Scopes: `model`,
`state`, `runtime`, `scheduling`, `migration`, `storage`, `backup`, `network`, `identity`,
`recipe`, `api`, `web`, `cli`, `chat`, `streams`, `observability`, `extensions`, `security`,
`operations`, `agent`, `helper`, `store`, `deps`. The body has Problem, Change and Verification
(the commands run and what they showed) and ends with `Closes #<issue>`. Commits are signed off
(`git commit -s`) and carry no `Co-Authored-By`, "Generated with" or session lines. Pull
requests are squash-merged.
