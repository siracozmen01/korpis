#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Installs the pinned tools into ./.tools: the code generators; oras, which publishes the recipes
# the end-to-end scripts run (scripts/e2e-phase0.sh); and Firecracker and its jailer, which the
# microVM tier's tests boot guests with (§3 of docs/04-runtimes.md). Generated code is committed,
# so building Korpis needs none of these; only changing a .proto does. Versions move only in a
# reviewed commit.
set -euo pipefail
cd "$(dirname "$0")/.."
TOOLS="$PWD/.tools"
BUF_VERSION=1.73.0
BUFFA_VERSION=0.9.2
CONNECT_CODEGEN_VERSION=0.9.0
SQLX_CLI_VERSION=0.9.0
ORAS_VERSION=1.3.4
FIRECRACKER_VERSION=1.17.0
# The release archives' digests, as the release's own .sha256.txt files state them, pinned here
# so that an asset replaced after this commit is refused rather than trusted.
FIRECRACKER_SHA256_x86_64=06094a1108ae9e82aa4c23a775aa92758f53f1175d422270d9d6162cb9ade558
FIRECRACKER_SHA256_aarch64=e351ebe4f7a16b5873bbd51005d2e6767103cff4d5ebc829df2d3f95a93e2256

mkdir -p "$TOOLS/bin"
if ! "$TOOLS/bin/buf" --version 2>/dev/null | grep -qx "$BUF_VERSION"; then
    arch=$(uname -m)
    base="https://github.com/bufbuild/buf/releases/download/v$BUF_VERSION"
    tmp=$(mktemp -d)
    curl -sSfL -o "$tmp/buf" "$base/buf-Linux-$arch"
    curl -sSfL -o "$tmp/sha256.txt" "$base/sha256.txt"
    (cd "$tmp" && grep " buf-Linux-$arch\$" sha256.txt | sed "s/buf-Linux-$arch/buf/" \
        | sha256sum -c --quiet)
    install -m 0755 "$tmp/buf" "$TOOLS/bin/buf"
    rm -rf "$tmp"
fi
if ! "$TOOLS/bin/oras" version 2>/dev/null | grep -q "Version: *$ORAS_VERSION\$"; then
    case "$(uname -m)" in x86_64) arch=amd64 ;; aarch64) arch=arm64 ;; *) arch=$(uname -m) ;; esac
    file="oras_${ORAS_VERSION}_linux_$arch.tar.gz"
    base="https://github.com/oras-project/oras/releases/download/v$ORAS_VERSION"
    tmp=$(mktemp -d)
    curl -sSfL -o "$tmp/$file" "$base/$file"
    curl -sSfL -o "$tmp/checksums.txt" "$base/oras_${ORAS_VERSION}_checksums.txt"
    (cd "$tmp" && grep " $file\$" checksums.txt | sha256sum -c --quiet)
    tar -xzf "$tmp/$file" -C "$tmp" oras
    install -m 0755 "$tmp/oras" "$TOOLS/bin/oras"
    rm -rf "$tmp"
fi
if ! "$TOOLS/bin/firecracker" --version 2>/dev/null | grep -qx "Firecracker v$FIRECRACKER_VERSION"
then
    arch=$(uname -m)
    case "$arch" in
        x86_64) sum=$FIRECRACKER_SHA256_x86_64 ;;
        aarch64) sum=$FIRECRACKER_SHA256_aarch64 ;;
        *) echo "no Firecracker release for $arch" >&2; exit 1 ;;
    esac
    file="firecracker-v$FIRECRACKER_VERSION-$arch.tgz"
    base="https://github.com/firecracker-microvm/firecracker/releases/download"
    base="$base/v$FIRECRACKER_VERSION"
    tmp=$(mktemp -d)
    curl -sSfL -o "$tmp/$file" "$base/$file"
    echo "$sum  $tmp/$file" | sha256sum -c --quiet
    tar -xzf "$tmp/$file" -C "$tmp"
    release="$tmp/release-v$FIRECRACKER_VERSION-$arch"
    install -m 0755 "$release/firecracker-v$FIRECRACKER_VERSION-$arch" "$TOOLS/bin/firecracker"
    install -m 0755 "$release/jailer-v$FIRECRACKER_VERSION-$arch" "$TOOLS/bin/jailer"
    rm -rf "$tmp"
fi
cargo install --locked --quiet --root "$TOOLS" protoc-gen-buffa --version "=$BUFFA_VERSION"
cargo install --locked --quiet --root "$TOOLS" protoc-gen-buffa-packaging \
    --version "=$BUFFA_VERSION"
cargo install --locked --quiet --root "$TOOLS" connectrpc-codegen \
    --version "=$CONNECT_CODEGEN_VERSION"
cargo install --locked --quiet --root "$TOOLS" sqlx-cli --version "=$SQLX_CLI_VERSION" \
    --no-default-features --features postgres,rustls
echo "tools ready in $TOOLS/bin"
