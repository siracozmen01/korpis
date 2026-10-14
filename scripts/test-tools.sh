#!/usr/bin/env bash
# Installs the servers some tests run against into ./.tools, pinned and verified: MinIO, the
# S3-compatible store the backup job's client and the STS-scoped backup target are tested
# against, run as a plain host process. MinIO publishes its community edition as source only, so
# it is built here from its release tag with a pinned Go toolchain, the toolchain checked against
# its published digest and every module against the Go checksum database. A test whose server
# is not installed says so and does not run.
set -euo pipefail
cd "$(dirname "$0")/.."
TOOLS="$PWD/.tools"
GO_VERSION=1.24.9
GO_SHA256_AMD64=5b7899591c2dd6e9da1809fde4a2fad842c45d3f6b9deb235ba82216e31e34a6
MINIO_VERSION=RELEASE.2025-10-15T17-29-55Z

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
echo "test servers ready in $TOOLS/bin"
