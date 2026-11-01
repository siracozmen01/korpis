#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The checks, in three tiers. Every tier runs the repository's rules first.
#
#   scripts/check.sh [changed]   before a push: formats, lints and tests the crates the branch
#                                touches since it left origin/main, and the panel if it changed
#   scripts/check.sh pr          what a pull request must pass: everything but the end-to-end
#                                suites, which need root and containerd
#   scripts/check.sh full        everything
#
# `changed` tests only the crates a change touches, not the crates that depend on them, so a
# change to a widely used crate passes it and can still fail `pr`. A change to the workspace's
# manifest, lockfile, toolchain, wire schema, migrations or conformance workspaces runs `pr`,
# since every crate is touched. Exits non-zero on the first failure; `pr` and `full` go through
# the shared machine's heavy-job wrapper (AGENTS.md).
set -euo pipefail
cd "$(dirname "$0")/.."
tier="${1:-changed}"
case "$tier" in changed|pr|full) ;; *) echo "usage: $0 [changed|pr|full]" >&2; exit 2 ;; esac
# A check builds each target once and compares nothing with the last build, so incremental
# compilation's caches, about half of the target directory, buy it nothing but disk.
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
skip_dirs=(--exclude-dir=.git --exclude-dir=target --exclude-dir=.tools --exclude-dir=node_modules
           --exclude-dir=dist)

rules() {
    echo "== em dash (AGENTS.md, Documentation)"
    if grep -rnPI "${skip_dirs[@]}" '\x{2014}' .; then exit 1; fi

    echo "== removed material"
    if grep -rnI "${skip_dirs[@]}" --exclude=check.sh -e '01-landscape' -e 'prior-art' .; then
        exit 1
    fi

    echo "== file length (AGENTS.md, Invariants)"
    local long
    long=$( (find crates scripts conformance -name '*.rs' -not -path '*/target/*'
              find web/src -name '*.ts' -o -name '*.tsx' | grep -v '^web/src/gen/') \
        | xargs awk 'FNR == 1 && prev && n > 400 { print prev ": " n " lines" }
                     { prev = FILENAME; n = FNR }
                     END { if (n > 400) print prev ": " n " lines" }')
    if [ -n "$long" ]; then echo "$long"; exit 1; fi

    echo "== licence lines (AGENTS.md, Invariants)"
    licences

    echo "== layers (AGENTS.md, Invariants)"
    ./scripts/check-layers.sh

    echo "== documentation (AGENTS.md, Documentation)"
    node scripts/check-docs.mjs
}

licences() {
    # Untracked files too, so a new file is checked before it is committed; a file deleted and not
    # yet staged is still listed, and is not there to check. The containerd API is vendored with
    # its own headers.
    local spdx
    spdx=$(git ls-files --cached --others --exclude-standard \
            'crates/*.rs' 'crates/*.sql' 'crates/*.proto' 'scripts/*.sh' 'scripts/*.mjs' \
            'conformance/*.rs' 'conformance/*.mjs' 'web/*.ts' 'web/*.tsx' 'web/*.js' \
            'web/*.html' 'web/*.css' \
        | grep -v '^crates/korpis-containerd/proto/' \
        | while read -r f; do
            [ -e "$f" ] || continue
            case "$f" in
                crates/korpis-proto/*|crates/korpis-containerd/*|crates/korpis-extension/*|\
                crates/korpis-driver-conformance/*|crates/korpis-conformance-probe/*|web/src/gen/*)
                    want=Apache-2.0 ;;
                *) want=AGPL-3.0-or-later ;;
            esac
            head -3 "$f" | grep -q "SPDX-License-Identifier: $want\$" || echo "$f: wants $want"
        done)
    if [ -n "$spdx" ]; then echo "$spdx"; exit 1; fi
}

wire_schema() {
    echo "== wire schema"
    if [ ! -x .tools/bin/buf ]; then
        echo "buf not installed (scripts/tools.sh), the schema is not checked"; return
    fi
    (cd crates/korpis-proto && ../../.tools/bin/buf lint)
    local crate fresh
    for crate in korpis-proto korpis-containerd; do
        fresh=$(mktemp)
        (cd "crates/$crate" && ../../.tools/bin/buf build --as-file-descriptor-set -o "$fresh")
        if ! cmp -s "$fresh" "crates/$crate/descriptor.binpb"; then
            rm -f "$fresh"
            echo "crates/$crate/descriptor.binpb is stale: run scripts/gen-proto.sh"; exit 1
        fi
        rm -f "$fresh"
    done
}

query_metadata() {
    echo "== query metadata (sqlx offline data matches the schema)"
    if [ ! -x .tools/bin/sqlx ] || [ -n "${KORPIS_TEST_PG_URL:-}" ]; then
        echo "sqlx-cli not installed (scripts/tools.sh), the query metadata is not checked"; return
    fi
    local admin check_db
    admin=$(./scripts/dev-pg.sh up)
    check_db="${admin%/*}/korpis_check"
    psql "$admin" -qc "DROP DATABASE IF EXISTS korpis_check WITH (FORCE)" >/dev/null
    psql "$admin" -qc "CREATE DATABASE korpis_check TEMPLATE template0" >/dev/null
    DATABASE_URL="$check_db" .tools/bin/sqlx migrate run --source crates/korpis-store/migrations \
        >/dev/null
    DATABASE_URL="$check_db" PATH="$PWD/.tools/bin:$PATH" cargo sqlx prepare --workspace --check
}

services() {
    if [ -z "${KORPIS_TEST_PG_URL:-}" ]; then ./scripts/dev-pg.sh up >/dev/null; fi
    if [ -z "${KORPIS_TEST_REGISTRY:-}" ]; then ./scripts/dev-registry.sh up >/dev/null; fi
}

deny() {
    if command -v cargo-deny >/dev/null; then
        echo "== cargo deny"
        cargo deny --locked check
        cargo deny "${conformance[@]}" --locked check
    else
        echo "== cargo deny: not installed (cargo install --locked cargo-deny), not run"
    fi
}

# Only the crates and the panel the branch touches: committed since it left origin/main, staged,
# unstaged and untracked. Without origin/main, the last commit's changes.
changed() {
    local base files crates packages=() tested=() crate
    base=$(git merge-base HEAD origin/main 2>/dev/null || git rev-parse HEAD~1)
    files=$( (git diff --name-only "$base"; git ls-files --others --exclude-standard) | sort -u)
    if grep -qE '^(Cargo\.(toml|lock)|rust-toolchain\.toml|clippy\.toml|deny\.toml|\.sqlx/|conformance/|crates/korpis-(proto|containerd)/.*\.proto$|crates/korpis-store/migrations/)' \
        <<<"$files"; then
        echo "== the change reaches every crate: running the pr tier"
        exec "$0" pr
    fi
    if grep -q '^web/' <<<"$files"; then
        echo "== web client"
        ./scripts/check-web.sh
    fi
    crates=$(grep -oE '^crates/[^/]+' <<<"$files" | sort -u | sed 's#^crates/##' || true)
    for crate in $crates; do
        [ -f "crates/$crate/Cargo.toml" ] || continue
        packages+=(-p "$crate")
        # The end-to-end suites belong to the full tier however small the change to them.
        [ "$crate" = korpis-e2e ] || tested+=(-p "$crate")
    done
    if [ ${#packages[@]} -eq 0 ]; then echo "== no crate changed"; return; fi
    export SQLX_OFFLINE=true
    echo "== rustfmt"
    cargo fmt --all --check
    echo "== clippy:" "${packages[@]}"
    cargo clippy "${packages[@]}" --all-targets --all-features --locked -- -D warnings
    if [ ${#tested[@]} -eq 0 ]; then return; fi
    echo "== tests:" "${tested[@]}"
    services
    cargo test "${tested[@]}" --all-features --locked
}

# The conformance packages are workspaces of their own (conformance/webauthn/Cargo.toml says why),
# checked like the main one into a target directory of their own.
conformance=(--manifest-path conformance/webauthn/Cargo.toml)
conformance_target=(--target-dir target/conformance)

workspace() {
    wire_schema
    echo "== web client"
    ./scripts/check-web.sh
    query_metadata
    # Everything below builds from the committed query metadata, as a fresh checkout would.
    export SQLX_OFFLINE=true

    echo "== rustfmt"
    cargo fmt --all --check
    cargo fmt "${conformance[@]}" --check

    echo "== clippy"
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    # The helper links the domain without its authority module; that build must stay clean too.
    cargo clippy -p korpis-core --no-default-features --locked -- -D warnings
    cargo clippy "${conformance[@]}" "${conformance_target[@]}" --all-targets --locked -- -D warnings

    echo "== tests (database: scripts/dev-pg.sh, registry: scripts/dev-registry.sh)"
    services
    # The OIDC provider is tested against an independent issuer, an npm package held to the
    # panel's supply-chain policy: exact versions, no install scripts, and deny.toml's licences.
    if [ -d conformance/oidc/node_modules/oidc-provider ]; then
        (cd conformance/oidc && pnpm licenses list --json | node ../../scripts/web-licences.mjs)
    else
        echo "the OIDC issuer is not installed (scripts/test-tools.sh), its conformance is not run"
    fi
    cargo test --workspace --exclude korpis-e2e --all-features --locked
    cargo test "${conformance[@]}" "${conformance_target[@]}" --locked
}

end_to_end() {
    echo "== end to end"
    if [ ! -d .tools/ms-playwright ]; then
        echo "Chromium not installed (scripts/test-tools.sh), the panel's browser tests are not run"
    fi
    # The end-to-end tests run the `korpis` binary as a user would, built as its own package and
    # so with its own features, not the workspace's; built here first, a test's build of it is a
    # link rather than minutes of compiling under the nodes the tests are running, which a node
    # starved past its lease's term would lose its runs to.
    cargo build -q -p korpis-cli --bin korpis --locked
    # So are the programs they run in containers, built static by the test that names them all.
    cargo test -q -p korpis-e2e --all-features --locked --test e2e -- \
        programs::every_program_is_built_static --exact
    cargo test -p korpis-e2e --all-features --locked
}

rules
case "$tier" in
    changed) changed ;;
    pr) workspace; deny ;;
    full) workspace; end_to_end; deny ;;
esac
echo "all checks passed"
