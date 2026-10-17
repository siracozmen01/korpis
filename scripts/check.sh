#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Every check a commit must pass. Run from anywhere; exits non-zero on the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== em dash (CLAUDE.md rule 2)"
if grep -rnPI --exclude-dir=.git --exclude-dir=target --exclude-dir=.tools --exclude-dir=node_modules \
    --exclude-dir=dist '\x{2014}' .; then exit 1; fi

echo "== removed material"
if grep -rnI --exclude-dir=.git --exclude-dir=target --exclude-dir=.tools --exclude-dir=node_modules \
    --exclude-dir=dist --exclude=CLAUDE.md --exclude=check.sh \
    -e '01-landscape' -e 'prior-art' .; then exit 1; fi

echo "== file length (400 lines, CONTRIBUTING.md \"Code rules\")"
long=$( (find crates scripts conformance -name '*.rs' -not -path '*/target/*'
          find web/src -name '*.ts' -o -name '*.tsx' | grep -v '^web/src/gen/') \
    | xargs awk 'FNR == 1 && prev && n > 400 { print prev ": " n " lines" }
                 { prev = FILENAME; n = FNR }
                 END { if (n > 400) print prev ": " n " lines" }')
if [ -n "$long" ]; then echo "$long"; exit 1; fi

echo "== licence lines (CLAUDE.md, §2 of docs/19-governance.md)"
# Untracked files too, so a new file is checked before it is committed; a file deleted and not
# yet staged is still listed, and is not there to check. The containerd API is vendored with its
# own headers.
spdx=$(git ls-files --cached --others --exclude-standard \
        'crates/*.rs' 'crates/*.sql' 'crates/*.proto' 'scripts/*.sh' 'scripts/*.mjs' \
        'conformance/*.rs' 'web/*.ts' 'web/*.tsx' 'web/*.js' 'web/*.html' 'web/*.css' \
    | grep -v '^crates/korpis-containerd/proto/' \
    | while read -r f; do
        [ -e "$f" ] || continue
        case "$f" in
            crates/korpis-proto/*|crates/korpis-containerd/*|crates/korpis-extension/*|web/src/gen/*)
                want=Apache-2.0 ;;
            *) want=AGPL-3.0-or-later ;;
        esac
        head -3 "$f" | grep -q "SPDX-License-Identifier: $want\$" || echo "$f: wants $want"
    done)
if [ -n "$spdx" ]; then echo "$spdx"; exit 1; fi

echo "== layers (ARCHITECTURE.md §3)"
./scripts/check-layers.sh

echo "== wire schema"
if [ -x .tools/bin/buf ]; then
    (cd crates/korpis-proto && ../../.tools/bin/buf lint)
    fresh=$(mktemp)
    (cd crates/korpis-proto && ../../.tools/bin/buf build --as-file-descriptor-set -o "$fresh")
    if ! cmp -s "$fresh" crates/korpis-proto/descriptor.binpb; then
        rm -f "$fresh"
        echo "descriptor.binpb is stale: run scripts/gen-proto.sh"; exit 1
    fi
    rm -f "$fresh"
    fresh=$(mktemp)
    (cd crates/korpis-containerd && ../../.tools/bin/buf build --as-file-descriptor-set -o "$fresh")
    if ! cmp -s "$fresh" crates/korpis-containerd/descriptor.binpb; then
        rm -f "$fresh"
        echo "korpis-containerd/descriptor.binpb is stale: run scripts/gen-proto.sh"; exit 1
    fi
    rm -f "$fresh"
else
    echo "buf not installed (scripts/tools.sh), schema lint skipped"
fi

echo "== web client"
./scripts/check-web.sh

echo "== query metadata (sqlx offline data matches the schema)"
if [ -x .tools/bin/sqlx ] && [ -z "${KORPIS_TEST_PG_URL:-}" ]; then
    admin=$(./scripts/dev-pg.sh up)
    check_db="${admin%/*}/korpis_check"
    psql "$admin" -qc "DROP DATABASE IF EXISTS korpis_check WITH (FORCE)" >/dev/null
    psql "$admin" -qc "CREATE DATABASE korpis_check TEMPLATE template0" >/dev/null
    DATABASE_URL="$check_db" .tools/bin/sqlx migrate run --source crates/korpis-store/migrations >/dev/null
    DATABASE_URL="$check_db" PATH="$PWD/.tools/bin:$PATH" cargo sqlx prepare --workspace --check
else
    echo "sqlx-cli not installed (scripts/tools.sh), query metadata check skipped"
fi
# Everything below builds from the committed query metadata, as a fresh checkout would.
export SQLX_OFFLINE=true

# The conformance packages are workspaces of their own (conformance/webauthn/Cargo.toml says why),
# checked like the main one into a target directory of their own.
conformance="--manifest-path conformance/webauthn/Cargo.toml"
conformance_target="--target-dir target/conformance"

echo "== rustfmt"
cargo fmt --all --check
cargo fmt $conformance --check

echo "== clippy"
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
# The helper links the domain without its authority module; that build must stay clean too.
cargo clippy -p korpis-core --no-default-features --locked -- -D warnings
cargo clippy $conformance $conformance_target --all-targets --locked -- -D warnings

echo "== tests (database: scripts/dev-pg.sh, registry: scripts/dev-registry.sh)"
if [ -z "${KORPIS_TEST_PG_URL:-}" ]; then ./scripts/dev-pg.sh up >/dev/null; fi
if [ -z "${KORPIS_TEST_REGISTRY:-}" ]; then ./scripts/dev-registry.sh up >/dev/null; fi
# The panel's browser tests run with the rest when Chromium is installed; without it they are
# skipped, and said to be here, where a reader of the check sees it.
if [ ! -d .tools/ms-playwright ]; then
    echo "Chromium not installed (scripts/test-tools.sh), the panel's browser tests are skipped"
fi
cargo test --workspace --all-features --locked
cargo test $conformance $conformance_target --locked

if command -v cargo-deny >/dev/null; then
    echo "== cargo deny"
    cargo deny --locked check
    cargo deny $conformance --locked check
else
    echo "== cargo deny: not installed, skipped (cargo install --locked cargo-deny)"
fi
echo "all checks passed"
