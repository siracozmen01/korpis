# SPDX-License-Identifier: AGPL-3.0-or-later
# shellcheck shell=bash
# What the end-to-end scripts share (scripts/e2e-phase0.sh, scripts/e2e-phase1.sh): their output,
# their waiting, the processes they start, and leaving the machine as they found it, however they
# exit. Sourced by them, not run; it expects $REPO, and sets $BIN, $WORK, $PG, $REGISTRY, and $DB.

log() { printf '\n== %s\n' "$*" >&2; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# Fails unless this machine has what every end-to-end script needs, and the tools named.
needs() {
    [ "$(id -u)" = 0 ] || fail "needs root: the helper and containerd"
    [ -S /run/containerd/containerd.sock ] || fail "needs containerd at /run/containerd"
    for tool in docker psql jq ctr "$@"; do
        command -v "$tool" >/dev/null || fail "needs $tool"
    done
    ORAS=$REPO/.tools/bin/oras
    [ -x "$ORAS" ] || fail "needs oras: run scripts/tools.sh"
}

# Waits up to $1 seconds for the command after it to succeed.
until_ok() {
    local secs=$1; shift
    for _ in $(seq 1 $((secs * 2))); do
        if "$@" >/dev/null 2>&1; then return 0; fi
        sleep 0.5
    done
    "$@"
}

PIDS=()
WORKLOADS=()
# Starts the command after $1 in the background, logging to $WORK/$1.log; its pid is $LAST.
start() {
    local name=$1; shift
    "$@" >>"$WORK/$name.log" 2>&1 &
    LAST=$!
    PIDS+=("$LAST")
}

# The development database and registry, a database of this run's own, and a directory for it.
begin() {
    BIN=$REPO/target/debug
    WORK=$(mktemp -d /tmp/korpis-e2e.XXXXXX)
    PG=$(scripts/dev-pg.sh up)
    REGISTRY=$(scripts/dev-registry.sh up)
    DB="korpis_e2e_$$"
    trap leave EXIT
}

# Leaves the machine as it was: the processes stopped, then whatever the node made that it did
# not remove itself, the containers of the workloads in $WORKLOADS and every network under the
# node's netns root (the helper's own release, as for a machine whose node is gone, §3 of
# docs/18-operations.md), then the volumes' filesystem if one was mounted, the job cgroups, the
# database, and the files.
leave() {
    set +e
    for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null; done
    wait 2>/dev/null
    for w in "${WORKLOADS[@]}"; do
        for id in $(ctr -n korpis containers ls -q "labels.\"korpis.workload\"==$w" 2>/dev/null)
        do
            ctr -n korpis tasks kill -s SIGKILL "$id" >/dev/null 2>&1
            sleep 0.3
            ctr -n korpis tasks rm -f "$id" >/dev/null 2>&1
            ctr -n korpis containers rm "$id" >/dev/null 2>&1
            ctr -n korpis snapshots rm "$id" >/dev/null 2>&1
        done
    done
    if [ -d "${NODE:-}/netns" ]; then
        "$BIN/korpis-helper" release-networks --netns-root "$NODE/netns" >&2
    fi
    if [ -n "${MOUNTED:-}" ]; then umount -l "$MOUNTED"; fi
    if [ -n "${CGROUPS:-}" ]; then
        for _ in $(seq 1 20); do
            find "$CGROUPS" -depth -type d -exec rmdir {} + 2>/dev/null
            [ -d "$CGROUPS" ] || break
            sleep 0.1
        done
    fi
    psql "$PG" -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" >/dev/null 2>&1
    rm -rf "$WORK" "${BACKUP:-}"
}

# A control plane installed into a database of this run's own and serving on a port of its own,
# the `korpis` command pointed at it as root, and an organization that runs unsigned recipes,
# with a project `games`; the organization is $ORG.
control_plane() {
    psql "$PG" -qc "CREATE DATABASE $DB TEMPLATE template0" >/dev/null
    local url="${PG%/*}/$DB" token login port
    # The processes are started here rather than by systemd, so installation writes no units;
    # the control plane's backup target is a directory on a tmpfs, another device than $WORK's.
    BACKUP=$(mktemp -d /dev/shm/korpis-e2e-backup.XXXXXX)
    "$BIN/korpis-server" install --data-dir "$WORK/server" --database-owner-url "$url" \
        --organization acme --root-token --backup-target "$BACKUP" --no-system \
        >"$WORK/install.txt"
    token=$(sed -n '/root API token is shown once/{n;n;s/^ *//p;q}' "$WORK/install.txt")
    [ -n "$token" ] || fail "install printed no root token"
    login=$(sed -n 's/^  --database-url //p' "$WORK/install.txt")
    [ -n "$login" ] || fail "install printed no login for the control plane"
    port=$((20000 + $$ % 20000))
    start server "$BIN/korpis-server" serve --data-dir "$WORK/server" --database-url "$login" \
        --listen "127.0.0.1:$port" --plain-http-registry "$REGISTRY"
    export KORPIS_SERVER="https://127.0.0.1:$port" KORPIS_CA="$WORK/server/tls/ca.pem"
    export KORPIS_TOKEN=$token
    until_ok 30 korpis version
    ORG=$(korpis --json get organizations | jq -r '.[0].id')
    korpis call OrganizationService/SetTrustPolicy \
        -d "{\"organization\": \"$ORG\", \"policy\": {\"allowUnsigned\": true}}" >/dev/null
    korpis call ProjectService/CreateProject -d "{\"organization\": \"$ORG\", \"name\": \"games\"}" \
        >/dev/null
}

# The `korpis` command as an operator runs it, the development registry spoken to over HTTP.
korpis() { "$BIN/korpis" --plain-http "$REGISTRY" "$@"; }

# Enrolls this machine as node $1 and starts its helper and driver, as their units would: their
# volumes under $VOLUMES ($NODE/volumes unless set), their workloads' networks bound under
# $NODE/netns, and the helper's jobs in a cgroup of this run's own. The agent is the script's to
# start, since what it is told differs.
enroll() {
    NODE="$WORK/node"
    VOLUMES=${VOLUMES:-$NODE/volumes}
    CGROUPS=/sys/fs/cgroup/korpis-e2e-$$
    mkdir -p "$NODE"
    local join
    join=$(korpis --json node token create "$1")
    "$BIN/korpis-agent" --state-dir "$NODE/agent" join --server "$KORPIS_SERVER" \
        --token "$(jq -r .token <<<"$join")" --ca-fingerprint "$(jq -r .caFingerprint <<<"$join")" \
        --hostname "$1" >/dev/null
    start helper "$BIN/korpis-helper" --socket "$NODE/helper.sock" --volume-root "$VOLUMES" \
        --state "$NODE/helper" --content "$NODE/agent/content" --netns-root "$NODE/netns" \
        --job-cgroups "$CGROUPS" --ingress-binary none
    start driver "$BIN/korpis-driver-oci" --socket "$NODE/driver.sock" \
        --content "$NODE/agent/content" --volume-root "$VOLUMES" \
        --identity-root "$NODE/agent/identity" --netns-root "$NODE/netns" \
        --state-dir "$NODE/driver"
    until_ok 10 test -S "$NODE/helper.sock"
    until_ok 10 test -S "$NODE/driver.sock"
}
