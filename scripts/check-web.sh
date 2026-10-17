#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The web client's checks (§9 of docs/21-stack.md): installed from the lockfile with every install
# script refused, its generated API client current with the schema, type-checked, linted, tested,
# built, and its licences within deny.toml's list. Node and pnpm are required, as cargo is.
set -euo pipefail
cd "$(dirname "$0")/../web"
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0

pnpm install --frozen-lockfile --ignore-scripts --reporter=silent
fresh=$(mktemp -d)
trap 'rm -rf "$fresh"' EXIT
../.tools/bin/buf generate --template buf.gen.yaml --output "$fresh"
if ! diff -r "$fresh/src/gen" src/gen >/dev/null; then
    echo "web/src/gen is stale: run (cd web && pnpm generate) and commit it"; exit 1
fi
pnpm --silent typecheck
pnpm --silent lint
pnpm --silent format:check
pnpm --silent test
pnpm --silent exec vite build --logLevel warn
pnpm licenses list --json | node ../scripts/web-licences.mjs
