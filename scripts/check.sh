#!/usr/bin/env bash
# Every check a commit must pass. Run from anywhere; exits non-zero on the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== em dash (CLAUDE.md rule 2)"
if grep -rnPI --exclude-dir=.git --exclude-dir=target --exclude-dir=.tools '\x{2014}' .; then exit 1; fi

echo "== removed material"
if grep -rnI --exclude-dir=.git --exclude-dir=target --exclude-dir=.tools --exclude=CLAUDE.md --exclude=check.sh \
    -e '01-landscape' -e 'prior-art' .; then exit 1; fi

echo "== file length (400 lines, CONTRIBUTING.md \"Code rules\")"
long=$(find crates scripts -name '*.rs' -not -path '*/target/*' \
    -exec awk 'END { if (NR > 400) print FILENAME ": " NR " lines" }' {} \;)
if [ -n "$long" ]; then echo "$long"; exit 1; fi

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
else
    echo "buf not installed (scripts/tools.sh), schema lint skipped"
fi

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

echo "== rustfmt"
cargo fmt --all --check

echo "== clippy"
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
# The helper links the domain without its authority module; that build must stay clean too.
cargo clippy -p korpis-core --no-default-features --locked -- -D warnings

echo "== tests (database: scripts/dev-pg.sh)"
if [ -z "${KORPIS_TEST_PG_URL:-}" ]; then ./scripts/dev-pg.sh up >/dev/null; fi
cargo test --workspace --all-features --locked

if command -v cargo-deny >/dev/null; then
    echo "== cargo deny"
    cargo deny --locked check
else
    echo "== cargo deny: not installed, skipped (cargo install --locked cargo-deny)"
fi
echo "all checks passed"
