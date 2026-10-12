#!/usr/bin/env bash
# Rebuilds crates/korpis-proto/descriptor.binpb from the .proto files with the pinned buf. The
# Rust code is generated from that descriptor set at build time (crates/korpis-proto/build.rs),
# so building Korpis needs neither protoc nor buf; only changing a .proto does. scripts/check.sh
# fails if the committed descriptor set is stale.
set -euo pipefail
cd "$(dirname "$0")/.."
./scripts/tools.sh >/dev/null
export PATH="$PWD/.tools/bin:$PATH"
cd crates/korpis-proto
buf lint
buf build --as-file-descriptor-set -o descriptor.binpb
echo "wrote crates/korpis-proto/descriptor.binpb"
