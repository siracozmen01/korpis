#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Puts what the microvm tier runs where `korpis-server install` looks for it (§2 of
# docs/18-operations.md): Firecracker and its jailer, the release scripts/tools.sh pins by its
# published digest; the guest kernel, built by scripts/guest-kernel.sh from the release it pins;
# and korpis-guest, built static for the musl target. Each is installed root's, and its SHA-256
# printed, so what a node runs can be compared with what another built.
#
#   scripts/microvm-files.sh [DIR]    (default: /usr/lib/korpis)
#
# Run it as root on the node, from a checkout of the commit the node's other binaries are built
# from, after scripts/tools.sh and scripts/guest-kernel.sh; then run `korpis-server install`
# again, which finds the files and the node runs the firecracker driver beside the oci driver.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
DIR=${1:-/usr/lib/korpis}
TRIPLE=x86_64-unknown-linux-musl

fail() { printf 'microvm-files.sh: %s\n' "$*" >&2; exit 1; }

[ "$(uname -m)" = x86_64 ] || fail "no guest kernel configuration for $(uname -m) yet"
version=$(sed -n 's/^FIRECRACKER_VERSION=//p' scripts/tools.sh)
"$REPO/.tools/bin/firecracker" --version 2>/dev/null | grep -qx "Firecracker v$version" ||
    fail "Firecracker v$version is not in .tools/bin: run scripts/tools.sh"
[ -x "$REPO/.tools/bin/jailer" ] || fail "the jailer is not in .tools/bin: run scripts/tools.sh"
kernel=$(sed -n 's/^VERSION=//p' scripts/guest-kernel.sh)
[ -f "$REPO/.tools/guest/vmlinux-$kernel" ] ||
    fail "the guest kernel $kernel is not built: run scripts/guest-kernel.sh"

cargo build --quiet --locked --release -p korpis-guest --bin korpis-guest --target "$TRIPLE"

install -d -m 0755 "$DIR"
install -m 0755 "$REPO/.tools/bin/firecracker" "$DIR/firecracker"
install -m 0755 "$REPO/.tools/bin/jailer" "$DIR/jailer"
install -m 0644 "$REPO/.tools/guest/vmlinux-$kernel" "$DIR/vmlinux"
install -m 0755 "$REPO/target/$TRIPLE/release/korpis-guest" "$DIR/korpis-guest"
(cd "$DIR" && sha256sum firecracker jailer vmlinux korpis-guest)
