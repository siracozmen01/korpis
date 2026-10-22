#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Builds the kernel every microVM on a node boots (§6 of docs/04-runtimes.md): kernel.org's
# longterm release below, pinned by its SHA-256, configured with guest/kernel/ (whose README
# records where each file came from), into .tools/guest/ unless a directory is named. The build
# is stamped with fixed values rather than this machine's clock, user, and name, so the same
# compiler rebuilds the same bytes. Versions move only in a reviewed commit. Needs a C compiler,
# make, flex, bison, bc, and libelf's and OpenSSL's headers.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
VERSION=6.18.58
SHA256=069b19f9032c4dac8149ae4b6520c35debca0f42a9f6d52ee55ba6ca9b818013
OUT=${1:-$REPO/.tools/guest}

case "$(uname -m)" in
    x86_64) config=microvm-kernel-ci-x86_64-6.18.config ;;
    *) echo "guest-kernel.sh: no guest configuration for $(uname -m) yet" >&2; exit 1 ;;
esac
if [ -f "$OUT/vmlinux-$VERSION" ]; then
    echo "guest kernel $VERSION ready in $OUT"
    exit 0
fi
for tool in cc make flex bison bc; do
    command -v "$tool" >/dev/null || { echo "guest-kernel.sh: needs $tool" >&2; exit 1; }
done

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -sSfL -o "$tmp/linux.tar.xz" \
    "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$VERSION.tar.xz"
echo "$SHA256  $tmp/linux.tar.xz" | sha256sum -c --quiet
tar -xJf "$tmp/linux.tar.xz" -C "$tmp"
cd "$tmp/linux-$VERSION"
awk 1 "$REPO/guest/kernel/$config" "$REPO/guest/kernel/ci.config" >.config
make -s olddefconfig
export KBUILD_BUILD_TIMESTAMP="1970-01-01 00:00:00 UTC" KBUILD_BUILD_USER=korpis \
    KBUILD_BUILD_HOST=korpis KBUILD_BUILD_VERSION=1
make -s -j "${KERNEL_JOBS:-8}" vmlinux
mkdir -p "$OUT"
install -m 0644 .config "$OUT/vmlinux-$VERSION.config"
install -m 0644 vmlinux "$OUT/vmlinux-$VERSION"
ln -sfn "vmlinux-$VERSION" "$OUT/vmlinux"
echo "guest kernel $VERSION ready in $OUT: $(sha256sum "$OUT/vmlinux-$VERSION" | cut -d' ' -f1)"
