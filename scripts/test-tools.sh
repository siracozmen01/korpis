#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Installs the servers some tests run against into ./.tools, pinned and verified: MinIO, the
# S3-compatible store the backup job's client and the STS-scoped backup target are tested
# against, and OpenBao, whose Transit engine the first-party key management provider is tested
# against, each run as a plain host process. OpenBao is installed from its release archive,
# checked against the digest pinned here, which its signed checksums list published. MinIO publishes its community edition as source only, so
# it is built here from its release tag with a pinned Go toolchain, the toolchain checked against
# its published digest and every module against the Go checksum database. A test whose server
# is not installed says so and does not run.
set -euo pipefail
cd "$(dirname "$0")/.."
TOOLS="$PWD/.tools"
GO_VERSION=1.24.9
GO_SHA256_AMD64=5b7899591c2dd6e9da1809fde4a2fad842c45d3f6b9deb235ba82216e31e34a6
MINIO_VERSION=RELEASE.2025-10-15T17-29-55Z
OPENBAO_VERSION=2.7.1
OPENBAO_SHA256_AMD64=0e2f1ce10d124e03112b50dd2fbec6b78003783253bc3a91587938f39d1e2243

mkdir -p "$TOOLS/bin"
if [ "$(cat "$TOOLS/minio.version" 2>/dev/null)" != "$MINIO_VERSION" ]; then
    case "$(uname -m)" in
        x86_64) arch=amd64; digest=$GO_SHA256_AMD64 ;;
        *) echo "no pinned Go toolchain for $(uname -m)" >&2; exit 1 ;;
    esac
    if ! "$TOOLS/go/bin/go" version 2>/dev/null | grep -q "go$GO_VERSION "; then
        file="go$GO_VERSION.linux-$arch.tar.gz"
        tmp=$(mktemp -d)
        curl -sSfL -o "$tmp/$file" "https://go.dev/dl/$file"
        echo "$digest  $tmp/$file" | sha256sum -c --quiet
        rm -rf "$TOOLS/go"
        tar -xzf "$tmp/$file" -C "$TOOLS"
        rm -rf "$tmp"
    fi
    GOTOOLCHAIN=local GOPATH="$TOOLS/gopath" GOBIN="$TOOLS/bin" GOFLAGS=-modcacherw \
        "$TOOLS/go/bin/go" install "github.com/minio/minio@$MINIO_VERSION"
    echo "$MINIO_VERSION" > "$TOOLS/minio.version"
fi
if [ "$(cat "$TOOLS/openbao.version" 2>/dev/null)" != "$OPENBAO_VERSION" ]; then
    case "$(uname -m)" in
        x86_64) arch=amd64; digest=$OPENBAO_SHA256_AMD64 ;;
        *) echo "no pinned OpenBao for $(uname -m)" >&2; exit 1 ;;
    esac
    file="openbao_${OPENBAO_VERSION}_linux_$arch.tar.gz"
    tmp=$(mktemp -d)
    curl -sSfL -o "$tmp/$file" \
        "https://github.com/openbao/openbao/releases/download/v$OPENBAO_VERSION/$file"
    echo "$digest  $tmp/$file" | sha256sum -c --quiet
    tar -xzf "$tmp/$file" -C "$tmp" bao
    install -m 0755 "$tmp/bao" "$TOOLS/bin/bao"
    rm -rf "$tmp"
    echo "$OPENBAO_VERSION" > "$TOOLS/openbao.version"
fi
echo "test servers ready in $TOOLS/bin"
