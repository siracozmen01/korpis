# Checks

`scripts/check.sh` runs the checks in three tiers, each a superset of the one before. Every tier
runs the repository's rules first and stops at the first failure.

| Tier | When | Runs | Budget |
|---|---|---|---|
| `changed` (the default) | before a push | the rules; the panel's checks if `web/` changed; rustfmt, clippy and tests for the crates the branch touches | 2 minutes |
| `pr` | before a pull request, and in CI | the rules; the generated files; every crate's clippy and tests except the end-to-end crate; the dependency policy | 10 minutes |
| `full` | before a release, and on demand | `pr` plus the end-to-end crate: nodes, containerd, microVMs and the panel's browser tests | 20 minutes |

A budget overrun is a defect in the tests, fixed like any other.

## The rules

Every tier checks that no file contains the em dash character or names removed material, that no
source file exceeds 400 lines, that every source file carries its SPDX line, and that the layer rule
holds (`scripts/check-layers.sh`).

## changed

`changed` diffs the branch against `git merge-base HEAD origin/main`, untracked files included. A
change to the workspace manifest or lockfile, the toolchain, `clippy.toml`, `deny.toml`, `.sqlx/`,
`conformance/`, a `.proto` or a migration touches every crate, so `changed` runs `pr` instead. A
change under `web/` runs `scripts/check-web.sh`. Each touched crate gets clippy with warnings denied
and its tests. `changed` tests only the crates a change touches, not those depending on them, so a
change to a widely used crate can pass `changed` and fail `pr`.

## pr

- the wire schema: `buf lint`, and each descriptor set built afresh and compared with the committed
  one (skipped, saying so, without `scripts/tools.sh`);
- the query metadata: a scratch database migrated and `cargo sqlx prepare --check` run against it;
- the panel: `scripts/check-web.sh` (install from the frozen lockfile, regenerate the client, type
  check, lint, component tests, build, licences);
- rustfmt, and clippy with warnings denied across the workspace and the conformance packages,
  including `korpis-core` built without its authority module, as the helper links it;
- every test but `korpis-e2e`, against the development PostgreSQL and registry, which the check
  starts if they are down, and the OIDC issuer's licences where it is installed;
- `cargo deny` against `deny.toml`.

## full

`full` adds the end-to-end crate: the `korpis` binary and the static programs tests run in
containers are built first, then every `korpis-e2e` test. These need root, containerd, KVM and the
tools of `scripts/test-tools.sh`; a test missing one says so and does not run. The browser tests run
when Playwright's Chromium is installed and say they are skipped when it is not.

## On a shared machine

`pr` and `full` compile and test the whole workspace. On a machine shared with other projects, run
them through the machine's heavy-job wrapper, one at a time per session, with `CARGO_BUILD_JOBS=4`,
and never build in a checkout while a check runs in it ([AGENTS.md](../../AGENTS.md)). The check
sets `CARGO_INCREMENTAL=0`, since a check compares nothing with the last build and incremental
caches are about half the target directory.
