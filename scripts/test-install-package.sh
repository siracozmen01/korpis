#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# `korpis-server install --postgres package` end to end (§2 of docs/18-operations.md), inside a
# container running systemd as its init, one per distribution family: the distribution's own
# PostgreSQL installed by its package manager, the owner made, the users and units written, the
# control plane started by its unit as its own user, and the panel's API answering. Then every
# unit is checked by systemd-analyze, and the node's helper socket and oci driver are started
# against the distribution's containerd; the agent is not, since it runs once joined.
#
#   scripts/test-install-package.sh [debian:13|ubuntu:24.04|fedora:42 ...]   (default: all three)
#
# Needs docker and the network (the distributions' package mirrors). The containers run
# privileged, as systemd in a container needs; each is removed on exit however it exits.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD

log() { printf '\n== %s\n' "$*" >&2; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

command -v docker >/dev/null || fail "needs docker"
DISTROS=("$@")
[ ${#DISTROS[@]} -gt 0 ] || DISTROS=(debian:13 ubuntu:24.04 fedora:42)

log "building"
cargo build --quiet -p korpis-server -p korpis-agent -p korpis-driver-oci -p korpis-helper
BIN=$REPO/target/debug
NAMES=()
cleanup() {
    for name in "${NAMES[@]}"; do docker rm -f "$name" >/dev/null 2>&1 || true; done
}
trap cleanup EXIT

# An image of $1 with systemd as its init.
image() {
    local base=$1 tag=korpis-install-test:${1//[:.]/-}
    local install
    case "$base" in
    fedora:*) install="dnf install -y systemd util-linux procps-ng curl containerd && dnf clean all" ;;
    *) install="apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y systemd systemd-sysv curl procps containerd && \
        rm -rf /var/lib/apt/lists/*" ;;
    esac
    docker build -q -t "$tag" - >/dev/null <<EOF
FROM $base
RUN $install
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
EOF
    echo "$tag"
}

# Runs its arguments in the container $NAME.
inside() { docker exec "$NAME" "$@"; }

for distro in "${DISTROS[@]}"; do
    log "$distro: an image with systemd"
    tag=$(image "$distro")
    NAME="korpis-install-test-$$-${distro//[:.]/-}"
    NAMES+=("$NAME")
    docker run -d --name "$NAME" --privileged --cgroupns=private --tmpfs /run --tmpfs /run/lock \
        --tmpfs /mnt/backup -v "$BIN:/opt/korpis:ro" "$tag" >/dev/null
    for _ in $(seq 1 60); do
        state=$(inside systemctl is-system-running 2>/dev/null || true)
        case "$state" in running | degraded) break ;; esac
        sleep 1
    done

    log "$distro: install, PostgreSQL from the package manager"
    inside /opt/korpis/korpis-server install --postgres package --organization acme --root-token \
        --first-node first --backup-target /mnt/backup --tls-name localhost \
        >"$REPO/target/install-$distro.txt" 2>&1 ||
        { cat "$REPO/target/install-$distro.txt"; fail "$distro: install failed"; }
    grep -q 'Korpis is installed' "$REPO/target/install-$distro.txt" || fail "$distro: no report"
    grep -q 'runuser -u korpis-agent -- korpis-agent join' "$REPO/target/install-$distro.txt" ||
        fail "$distro: no join command"
    [ "$(inside stat -c %a /etc/korpis/server.env)" = 600 ] || fail "$distro: server.env not 0600"
    [ "$(inside stat -c %a /etc/korpis/owner.env)" = 600 ] || fail "$distro: owner.env not 0600"
    inside grep -q '^KORPIS_DATABASE_URL=postgres://korpis_server:' /etc/korpis/server.env ||
        fail "$distro: the control plane does not have its own login"

    log "$distro: the units"
    inside systemd-analyze verify /etc/systemd/system/korpis-server.service \
        /etc/systemd/system/korpis-helper.socket /etc/systemd/system/korpis-helper.service \
        /etc/systemd/system/korpis-driver-oci.service /etc/systemd/system/korpis-agent.service \
        /etc/systemd/system/korpis-relay.service || fail "$distro: a unit does not verify"
    inside systemctl daemon-reload
    inside systemctl enable --now korpis-server.service korpis-helper.socket >/dev/null 2>&1 ||
        { inside journalctl -u korpis-server --no-pager | tail -30; fail "$distro: did not start"; }
    for _ in $(seq 1 60); do
        if inside curl -skf -o /dev/null https://localhost:7443/about/source; then break; fi
        sleep 1
    done
    inside curl -skf -o /dev/null https://localhost:7443/about/source ||
        { inside journalctl -u korpis-server --no-pager | tail -30; fail "$distro: not serving"; }
    user=$(inside ps -o user= -C korpis-server | head -n1)
    [ "$user" = korpis ] || fail "$distro: the control plane runs as '$user', not korpis"
    inside systemctl is-active --quiet korpis-helper.socket || fail "$distro: no helper socket"
    inside systemctl enable --now containerd.service korpis-driver-oci.service >/dev/null 2>&1 ||
        { inside journalctl -u korpis-driver-oci --no-pager | tail -30; fail "$distro: no driver"; }
    for _ in $(seq 1 30); do
        if inside test -S /run/korpis/driver-oci.sock; then break; fi
        sleep 1
    done
    inside test -S /run/korpis/driver-oci.sock ||
        { inside journalctl -u korpis-driver-oci --no-pager | tail -30; fail "$distro: no socket"; }
    [ "$(inside stat -c %G /run/korpis/driver-oci.sock)" = korpis-agent ] ||
        fail "$distro: the driver's socket is not the agent's group's"
    printf '%s: ok (%s)\n' "$distro" \
        "$(grep -o 'PostgreSQL [0-9.]*' "$REPO/target/install-$distro.txt" | sed -n 2p)"
done

log "installation from the package manager holds on ${DISTROS[*]}"
