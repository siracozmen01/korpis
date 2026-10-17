#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Installs the servers some tests run against into ./.tools, pinned and verified: MinIO, the
# S3-compatible store the backup job's client and the STS-scoped backup target are tested against;
# OpenBao, whose Transit engine the first-party key management provider is tested against; and
# Pebble, Let's Encrypt's test ACME server, with its challenge test server, which the control
# plane's certificate issuance is tested against; each run as a plain host process. OpenBao is
# installed from its release archive, checked against the digest pinned here, which its signed
# checksums list published. MinIO publishes its community edition as source only, and Pebble is
# built from source too, so both are built here from their release tags with a pinned Go toolchain,
# the toolchain checked against its published digest and every module against the Go checksum
# database. Chromium, for the panel's browser tests, is the build the pinned @playwright/test names,
# installed by Playwright's own installer into ./.tools/ms-playwright: that installer fetches it
# from Playwright's download host and checks nothing against a digest pinned here, which is the
# trust the browser tests place in the Playwright release itself. A test whose server is not
# installed says so and does not run.
set -euo pipefail
cd "$(dirname "$0")/.."
TOOLS="$PWD/.tools"
GO_VERSION=1.24.9
GO_SHA256_AMD64=5b7899591c2dd6e9da1809fde4a2fad842c45d3f6b9deb235ba82216e31e34a6
MINIO_VERSION=RELEASE.2025-10-15T17-29-55Z
OPENBAO_VERSION=2.7.1
OPENBAO_SHA256_AMD64=0e2f1ce10d124e03112b50dd2fbec6b78003783253bc3a91587938f39d1e2243
PEBBLE_VERSION=v2.10.1

mkdir -p "$TOOLS/bin"
# The pinned Go toolchain, checked against its published digest.
go_toolchain() {
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
}
# `go install` of each of its arguments with the pinned toolchain, every module checked against
# the Go checksum database.
go_install() {
    GOTOOLCHAIN=local GOPATH="$TOOLS/gopath" GOBIN="$TOOLS/bin" GOFLAGS=-modcacherw \
        "$TOOLS/go/bin/go" install "$@"
}
if [ "$(cat "$TOOLS/minio.version" 2>/dev/null)" != "$MINIO_VERSION" ]; then
    go_toolchain
    go_install "github.com/minio/minio@$MINIO_VERSION"
    echo "$MINIO_VERSION" > "$TOOLS/minio.version"
fi
if [ "$(cat "$TOOLS/pebble.version" 2>/dev/null)" != "$PEBBLE_VERSION" ]; then
    go_toolchain
    go_install "github.com/letsencrypt/pebble/v2/cmd/pebble@$PEBBLE_VERSION" \
        "github.com/letsencrypt/pebble/v2/cmd/pebble-challtestsrv@$PEBBLE_VERSION"
    echo "$PEBBLE_VERSION" > "$TOOLS/pebble.version"
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
PLAYWRIGHT_VERSION=$(node -p 'require("./web/package.json").devDependencies["@playwright/test"]')
if [ "$(cat "$TOOLS/playwright.version" 2>/dev/null)" != "$PLAYWRIGHT_VERSION" ]; then
    (cd web && pnpm install --frozen-lockfile --ignore-scripts --reporter=silent \
        && PLAYWRIGHT_BROWSERS_PATH="$TOOLS/ms-playwright" pnpm exec playwright install chromium)
    echo "$PLAYWRIGHT_VERSION" > "$TOOLS/playwright.version"
fi
echo "test servers ready in $TOOLS/bin, browsers in $TOOLS/ms-playwright"
