#!/usr/bin/env bash
# The dependency rule of ARCHITECTURE.md §3: dependencies point inward only. The compiler already
# makes an inward dependency impossible to forget; this makes an outward one impossible to add
# quietly. Reads the normal (non-dev) dependency graph of each layer and fails on a forbidden
# crate anywhere in it.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
# crate|features|forbidden crates (space separated)
rules=(
    "korpis-core|--all-features|tokio sqlx connectrpc korpis-proto korpis-app korpis-store hyper"
    "korpis-core|--no-default-features|tokio sqlx connectrpc korpis-proto biscuit-auth"
    "korpis-app||tokio sqlx connectrpc korpis-proto korpis-store hyper"
    "korpis-store||connectrpc korpis-proto korpis-server"
    "korpis-recipe||sqlx connectrpc korpis-proto"
    "korpis-wire||korpis-app sqlx korpis-store"
    "korpis-helper||tokio hyper connectrpc sqlx biscuit-auth korpis-app korpis-store"
    # The SDK is Apache-2.0 (§2 of docs/19-governance.md): it may link the wire contract and
    # nothing of ours that is AGPL, or every extension would inherit the AGPL.
    "korpis-extension||korpis-core korpis-app korpis-wire korpis-store korpis-recipe korpis-server korpis-agent sqlx"
    # Trust bootstrap is TLS alone: nothing of ours, no RPC stack, no runtime.
    "korpis-tls||korpis-core korpis-app korpis-wire korpis-store korpis-proto connectrpc tokio sqlx"
)
for rule in "${rules[@]}"; do
    IFS='|' read -r crate features forbidden <<<"$rule"
    # shellcheck disable=SC2086
    deps=$(cargo tree -p "$crate" $features -e normal --prefix none --format '{p}' --locked \
        | awk '{print $1}' | sort -u)
    for f in $forbidden; do
        if grep -qx "$f" <<<"$deps"; then
            echo "layer violation: $crate ${features:-(default features)} depends on $f"
            cargo tree -p "$crate" $features -e normal --locked -i "$f" | head -20
            fail=1
        fi
    done
done
exit $fail
