#!/usr/bin/env bash
# Installs the pinned tools into ./.tools: the code generators, and oras, which publishes the
# recipes the end-to-end script runs (scripts/e2e-phase0.sh). Generated code is committed, so
# building Korpis needs none of these; only changing a .proto does. Versions move only in a
# reviewed commit.
set -euo pipefail
cd "$(dirname "$0")/.."
TOOLS="$PWD/.tools"
BUF_VERSION=1.73.0
BUFFA_VERSION=0.9.2
CONNECT_CODEGEN_VERSION=0.9.0
SQLX_CLI_VERSION=0.9.0
ORAS_VERSION=1.3.4

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
cargo install --locked --quiet --root "$TOOLS" protoc-gen-buffa --version "=$BUFFA_VERSION"
cargo install --locked --quiet --root "$TOOLS" protoc-gen-buffa-packaging \
    --version "=$BUFFA_VERSION"
cargo install --locked --quiet --root "$TOOLS" connectrpc-codegen \
    --version "=$CONNECT_CODEGEN_VERSION"
cargo install --locked --quiet --root "$TOOLS" sqlx-cli --version "=$SQLX_CLI_VERSION" \
    --no-default-features --features postgres,rustls
echo "tools ready in $TOOLS/bin"
