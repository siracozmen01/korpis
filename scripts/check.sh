#!/usr/bin/env bash
# Every check a commit must pass. Run from anywhere; exits non-zero on the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== em dash (CLAUDE.md rule 2)"
if grep -rnPI --exclude-dir=.git --exclude-dir=target --exclude-dir=.tools '\x{2014}' .; then exit 1; fi

echo "== removed material"
if grep -rnI --exclude-dir=.git --exclude-dir=target --exclude-dir=.tools --exclude=CLAUDE.md --exclude=check.sh \
    -e '01-landscape' -e 'prior-art' .; then exit 1; fi

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

echo "== rustfmt"
cargo fmt --all --check

echo "== clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings

echo "== tests"
cargo test --workspace --locked

if command -v cargo-deny >/dev/null; then
    echo "== cargo deny"
    cargo deny --locked check
else
    echo "== cargo deny: not installed, skipped (cargo install --locked cargo-deny)"
fi
echo "all checks passed"
