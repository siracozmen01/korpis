#!/usr/bin/env bash
# Every check a commit must pass. Run from anywhere; exits non-zero on the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== em dash (CLAUDE.md rule 2)"
if grep -rnP --exclude-dir=.git --exclude-dir=target '\x{2014}' .; then exit 1; fi

echo "== removed material"
if grep -rn --exclude-dir=.git --exclude-dir=target --exclude=CLAUDE.md --exclude=check.sh \
    -e '01-landscape' -e 'prior-art' .; then exit 1; fi

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
