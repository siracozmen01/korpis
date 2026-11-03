#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The Phase 3 done-when, end to end, the way an operator runs it, every step through the `korpis`
# CLI: a running workload with a 40 GB volume moves between two nodes, keeps its public address,
# and its meter neither resets nor double-counts across the cutover.
#
# This machine stands in for two and a network: two network namespaces of the run's own, joined by a
# veth, each the machine of one node, whose helper serves inside it, so the nodes' workloads'
# networks, their dedicated addresses and the WireGuard mesh between them are there and nothing of
# them in this machine's own namespace; and a third, the client's network, joined to each. Each
# node's volumes are on a btrfs of its own with qgroups on, so the volume's 40 GB is a limit the
# kernel enforces. A control plane metering every minute is installed into a fresh database; the
# test extension, published as an image, serves an address router that routes a cluster pool's
# addresses where it is asked to and prints each route, which the client's network follows as an
# upstream network would. A workload serving its volume over HTTP on a dedicated address of that
# pool runs on one node; its volume is filled with $KORPIS_E2E_FILL_GIB GiB of random bytes (32 by
# default, the rest of the 40 GB left for the filesystem's own accounting) and hashed as written.
# Moved to the other node, it serves the same page at the same address, translated there and no
# longer where it was, its volume hashes the same, and each node's meter records of it follow one
# another without overlap, the next epoch beginning no earlier than the last ended. The hold is then
# released, the source's copy deleted, and everything deleted, the nodes removing what they made.
#
# Needs root, containerd, docker (for scripts/dev-pg.sh and scripts/dev-registry.sh), psql, jq,
# curl, nft, nsenter, mkfs.btrfs and btrfs (btrfs-progs), the kernel's WireGuard, oras from
# scripts/tools.sh, and about twice the fill free on the disk holding /tmp while it runs. Leaves
# nothing behind: its processes, containers, namespaces, filesystems, database, and files are
# removed on exit however it exits.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
# shellcheck source=scripts/e2e-lib.sh
. scripts/e2e-lib.sh
needs ip nft nsenter curl mkfs.btrfs btrfs sha256sum
FILL_GIB=${KORPIS_E2E_FILL_GIB:-32}

log "building"
cargo build --quiet --locked -p korpis-server -p korpis-agent -p korpis-driver-oci \
    -p korpis-helper -p korpis-cli -p korpis-stream -p korpis-files -p korpis-backup
# The router runs in a container that holds nothing but itself, so it is linked static, into a
# target directory of its own, as the end-to-end tests build it.
TRIPLE="$(uname -m)-unknown-linux-gnu"
CARGO_ENCODED_RUSTFLAGS=$'-C\x1ftarget-feature=+crt-static\x1f-C\x1fdebuginfo=0\x1f-C\x1fstrip=symbols' \
    RUSTFLAGS='' cargo build --quiet --locked -p korpis-extension --example test-extension \
    --target "$TRIPLE" --target-dir target/static
ROUTER_BINARY=$REPO/target/static/$TRIPLE/debug/examples/test-extension
begin

log "two machines joined by a network, and a client's network joined to each"
TAG=$(($$ % 100000))
for m in a b c; do
    ip netns add "korpis-p3-$TAG-$m"
    NETNSES+=("korpis-p3-$TAG-$m")
    ip -n "korpis-p3-$TAG-$m" link set lo up
done
A=/run/netns/korpis-p3-$TAG-a B=/run/netns/korpis-p3-$TAG-b C=/run/netns/korpis-p3-$TAG-c
in_ns() { local ns=$1; shift; nsenter "--net=$ns" "$@"; }
in_ns "$A" ip link add wireguard-probe type wireguard 2>/dev/null \
    || fail "this kernel makes no WireGuard link"
in_ns "$A" ip link del wireguard-probe
# Joins the namespaces $2 and $3 by a veth named for $1, at $4 and $5; a link's name is fifteen
# bytes at most.
wire() {
    local name=$1 one=$2 other=$3 one_address=$4 other_address=$5
    ip link add "p3$TAG${name}1" netns "${one##*/}" type veth peer name "p3$TAG${name}2" \
        netns "${other##*/}"
    in_ns "$one" ip addr add "$one_address" dev "p3$TAG${name}1"
    in_ns "$other" ip addr add "$other_address" dev "p3$TAG${name}2"
    in_ns "$one" ip link set "p3$TAG${name}1" up
    in_ns "$other" ip link set "p3$TAG${name}2" up
}
wire m "$A" "$B" 198.18.1.1/30 198.18.1.2/30
wire a "$C" "$A" 198.18.2.1/30 198.18.2.2/30
wire b "$C" "$B" 198.18.3.1/30 198.18.3.2/30
# The machines forward, as a node's does; this machine's own setting is left be.
for ns in "$A" "$B"; do in_ns "$ns" sysctl -qw net.ipv4.ip_forward=1; done
# Where the client's network sends what is routed to each machine.
declare -A GATEWAY=([a]=198.18.2.2 [b]=198.18.3.2)

log "installing the control plane, metering every minute"
SERVING=(--metering-interval 1m)
control_plane

log "publishing the router's image and its extension, and the server's recipe, with oras"
case "$(uname -m)" in aarch64) ARCH=arm64 ;; *) ARCH=amd64 ;; esac
mkdir -p "$WORK/image/root"
install -m 0755 "$ROUTER_BINARY" "$WORK/image/root/test-extension"
tar --owner=0 --group=0 --numeric-owner -C "$WORK/image/root" -cf "$WORK/image/layer.tar" \
    test-extension
cat >"$WORK/image/config.json" <<EOF
{"architecture": "$ARCH", "os": "linux", "config": {"Entrypoint": ["/test-extension"]},
 "rootfs": {"type": "layers",
            "diff_ids": ["sha256:$(sha256sum "$WORK/image/layer.tar" | cut -d' ' -f1)"]}}
EOF
IMAGE_REPO="$REGISTRY/e2e/test-router-$$"
IMAGE_DIGEST=$(cd "$WORK/image" && "$ORAS" push --plain-http --format json \
    --config config.json:application/vnd.oci.image.config.v1+json "$IMAGE_REPO:1.0.0" \
    layer.tar:application/vnd.oci.image.layer.v1.tar | jq -r .digest)
mkdir -p "$WORK/extension"
cat >"$WORK/extension/extension.yaml" <<EOF
apiVersion: korpis.dev/v1
kind: Extension
metadata: {name: test-router, version: "1.0.0", description: routes addresses, license: Apache-2.0}
runtime:
  targets:
    - {tier: container, arch: [amd64, arm64], image: "$IMAGE_REPO@$IMAGE_DIGEST"}
  entry:
    command: [/test-extension, --router]
preset:
  lifecycle: persistent
  console: logs
  resources:
    memory: {min: 32Mi, recommended: 64Mi}
    cpu: {min: 50m, recommended: 100m}
extension:
  api_version: korpis.extension.v1
  requires: [provider.serve, provider.address_router]
  provides:
    - {type: address_router, name: test}
EOF
EXTENSION="$REGISTRY/e2e/test-router-extension-$$"
(cd "$WORK/extension" && "$ORAS" push --plain-http \
    --artifact-type application/vnd.korpis.extension.v1 "$EXTENSION:1.0.0" \
    extension.yaml:application/vnd.korpis.extension.document.v1+yaml >/dev/null)
RECIPE="$REGISTRY/e2e/volume-server-$$"
mkdir -p "$WORK/recipe"
cat >"$WORK/recipe/recipe.yaml" <<EOF
apiVersion: korpis.dev/v1
kind: Recipe
metadata: {name: volume-server, version: "1.0.0", description: serves its volume, license: MIT}
runtime:
  targets:
    - {tier: container, arch: [amd64, arm64], image: "$REGISTRY/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"}
  entry:
    command: [/bin/sh, -c, 'echo serving; exec httpd -f -p 9000 -h /data']
preset:
  lifecycle: persistent
  console: logs
  volumes:
    - {name: data, path: /data, size_default: 64Mi}
  endpoints:
    - {name: web, protocol: tcp, port: 9000, exposure_default: dedicated_ip}
  resources:
    memory: {min: 32Mi, recommended: 64Mi}
    cpu:    {min: 50m, recommended: 100m}
EOF
(cd "$WORK/recipe" && "$ORAS" push --plain-http \
    --artifact-type application/vnd.korpis.recipe.v1 "$RECIPE:1.0.0" \
    recipe.yaml:application/vnd.korpis.recipe.document.v1+yaml >/dev/null)

log "enrolling two nodes, each on its machine, its volumes on a btrfs of its own"
declare -A NODE_DIR VOLUMES_OF NODE_ID MACHINE_OF
for m in a b; do
    at="$WORK/btrfs-$m"
    mkdir -p "$at"
    truncate -s 48G "$WORK/btrfs-$m.img"
    mkfs.btrfs -q "$WORK/btrfs-$m.img"
    mount -o loop "$WORK/btrfs-$m.img" "$at"
    MOUNTS+=("$at")
    btrfs quota enable "$at"
    btrfs quota rescan -w "$at" >/dev/null
    machine=A; [ "$m" = b ] && machine=B
    INSIDE=${!machine}
    enroll "p3-$m-$$" "$at/volumes"
    NODE_DIR[$m]=$NODE VOLUMES_OF[$m]=$VOLUMES
    address=198.18.1.1; [ "$m" = b ] && address=198.18.1.2
    # The agent reaches the control plane on this machine's loopback; what it advertises is its
    # machine's address, the mesh's endpoint. Its pool is the one enrollment gave it.
    start "agent-$m" "$BIN/korpis-agent" --state-dir "$NODE/agent" run \
        --driver-socket "$NODE/driver.sock" --helper-socket "$NODE/helper.sock" \
        --plain-http "$REGISTRY" --ingress false --advertise-address "$address" --ports "$PORTS"
done
unset INSIDE
for m in a b; do
    NODE_ID[$m]=$(korpis --json get nodes | jq -r --arg n "p3-$m-$$" '.[] | select(.name == $n) | .id')
    [ -n "${NODE_ID[$m]}" ] || fail "node p3-$m-$$ was never listed"
    MACHINE_OF[${NODE_ID[$m]}]=$m
done
meshed() {
    korpis --json get nodes \
        | jq -e '[.[] | select(.overlay == "OVERLAY_AVAILABLE")] | length == 2'
}
until_ok 120 meshed || fail "the nodes never reached each other through the mesh: \
$(korpis get nodes)"

log "the router installed as an extension, routing a cluster pool"
korpis --json extension install "$EXTENSION" --version 1.0.0 --workload-project games \
    >"$WORK/review.json"
DIGEST=$(jq -r .artifact.digest "$WORK/review.json")
korpis --json extension install "$EXTENSION" --version "$DIGEST" --approve \
    --workload-project games >"$WORK/installed.json"
INSTALLATION=$(jq -r .extension.id "$WORK/installed.json")
ROUTER=$(jq -r .extension.workload "$WORK/installed.json")
WORKLOADS+=("$ROUTER")
pinged() {
    korpis --json extension ping address_router test | jq -e '(.registration // "") != ""'
}
until_ok 180 pinged || fail "the router never answered: $(tail -n 20 "$WORK/agent-a.log")"
korpis address-pool create 198.18.9.0/29 --router test --routing bgp --reserve 198.18.9.1 \
    >/dev/null

log "korpis apply -f: a server with a 40 GB volume, on a dedicated address of that pool"
cat >"$WORK/server.yaml" <<EOF2
workload: server
project: games
recipe: $RECIPE:1.0.0
volumes:
  - {name: data, size: 40G}
EOF2
korpis apply -f "$WORK/server.yaml"
W=$(korpis --json get workloads games \
    | jq -r '.[] | select(.workload.name == "server") | .workload.id')
WORKLOADS+=("$W")
# The machine $W runs on, once it is observed running there.
runs_on() {
    korpis --json status "$W" >"$WORK/status.json" || return 1
    jq -e --arg n "${NODE_ID[$1]}" '.placement.node == $n
        and .observation.state == "OBSERVED_STATE_RUNNING"' "$WORK/status.json"
}
placed() { runs_on a || runs_on b; }
until_ok 180 placed || fail "the server never ran: $(korpis status "$W")"
HERE=a THERE=b
if ! runs_on a >/dev/null; then HERE=b THERE=a; fi
routed() {
    korpis --json status "$W" \
        | jq -e '(.dedicated.address // "") != "" and (.dedicated.waitingOnRouter | not)'
}
until_ok 120 routed || fail "its address was never routed: $(korpis status "$W")"
ADDRESS=$(korpis --json status "$W" | jq -r .dedicated.address)

# Routes the client's network as the router last said, and fetches the marker from $ADDRESS.
follows() {
    local said machine
    said=$(korpis logs "$ROUTER" --grep "route $ADDRESS " | grep -o 'nd_[0-9A-Za-z]*' | tail -n 1)
    machine=${MACHINE_OF[$said]:-}
    [ "$machine" = "$1" ] || return 1
    in_ns "$C" ip route replace "$ADDRESS/32" via "${GATEWAY[$machine]}"
}
answers() {
    [ "$(in_ns "$C" curl -sf --max-time 3 "http://$ADDRESS:9000/marker")" = "$MARKER" ]
}
# Whether the node on machine $1 translates connections to $ADDRESS.
# Read whole first: `grep -q` stopping early would fail the listing on a closed pipe.
translates() {
    local ruleset
    ruleset=$(in_ns "${NS_OF[$1]}" nft list ruleset)
    [[ $ruleset == *"$ADDRESS"* ]]
}
untranslated() { ! translates "$1"; }
declare -A NS_OF=([a]="$A" [b]="$B")

log "its volume filled with $FILL_GIB GiB of random bytes, hashed as written"
VOLUME="${VOLUMES_OF[$HERE]}/$W.data"
[ -d "$VOLUME" ] || fail "no volume at $VOLUME"
MARKER="served-$$-$RANDOM"
printf '%s' "$MARKER" >"$VOLUME/marker"
chmod 0644 "$VOLUME/marker"
FILLED=$(head -c "${FILL_GIB}G" /dev/urandom | tee "$VOLUME/filled" | sha256sum | cut -d' ' -f1)
sync -f "$VOLUME/filled"
df -h "${VOLUMES_OF[$HERE]%/volumes}" >&2

log "the client reaches it at $ADDRESS through the node it runs on"
until_ok 60 follows "$HERE" || fail "the router never routed $ADDRESS to $HERE: \
$(korpis logs "$ROUTER" | tail -n 5)"
until_ok 30 answers || fail "nothing answered at $ADDRESS"
translates "$HERE" || fail "$HERE does not translate $ADDRESS"
metered() {
    korpis --json get usage --workload "$W" --limit 1000 >"$WORK/usage.json" || return 1
    jq -e --arg n "${NODE_ID[$1]}" 'any(.[]; .node == $n)' "$WORK/usage.json"
}
until_ok 150 metered "$HERE" || fail "never metered on $HERE"

log "korpis workload move: to the other node, and the plan approved"
PLAN=$(korpis --json workload move "$W" --to "${NODE_ID[$THERE]}")
korpis plan approve "$(jq -r .id <<<"$PLAN")" >/dev/null
MIGRATION=$(korpis --json migration get "$W" | jq -r '.[0].id')
STARTED=$SECONDS
# Settled once it runs there, or once the migration ended without it, abandoned or rolled back.
settled() {
    runs_on "$THERE" || korpis --json migration get "$W" "$MIGRATION" \
        | jq -e '(.outcome // "MIGRATION_OUTCOME_UNSPECIFIED") != "MIGRATION_OUTCOME_UNSPECIFIED"'
}
until_ok 1800 settled || fail "it never settled: $(korpis migration get "$W" "$MIGRATION")"
runs_on "$THERE" >/dev/null || fail "it never ran on $THERE: $(korpis migration get "$W" \
    "$MIGRATION")"
log "moved in $((SECONDS - STARTED)) s"
korpis migration get "$W" "$MIGRATION" >&2

log "the same address, now routed to and translated by the node it moved to"
until_ok 60 follows "$THERE" || fail "the router never routed $ADDRESS to $THERE"
until_ok 30 answers || fail "nothing answered at $ADDRESS after the move"
[ "$(korpis --json status "$W" | jq -r .dedicated.address)" = "$ADDRESS" ] \
    || fail "the address changed: $(korpis status "$W")"
translates "$THERE" || fail "$THERE does not translate $ADDRESS"
until_ok 30 untranslated "$HERE" || fail "$HERE still translates $ADDRESS"

log "its volume, the same bytes"
MOVED="${VOLUMES_OF[$THERE]}/$W.data"
[ "$(sha256sum <"$MOVED/filled" | cut -d' ' -f1)" = "$FILLED" ] || fail "the volume differs"

log "its meter: each node's records follow one another, and the next epoch began after the last"
until_ok 150 metered "$THERE" || fail "never metered on $THERE"
# Each record's epoch, interval and node; a gap is the control plane's, under no node.
counted_once() {
    jq -e '
        def t: sub("\\.[0-9]+"; "") | fromdateiso8601;
        [.[] | {e: ((.leaseEpoch // "0") | tonumber), f: (.from | t), t: (.to | t),
                n: (.node // "")}] as $r
        | ([$r | group_by(.n)[] | sort_by(.f) | . as $g | range(1; length)
            | select($g[. - 1].t > $g[.].f)] | length == 0)
        and ([$r | map(select(.n != "")) | sort_by(.e, .f) | . as $m | range(1; length)
            | select($m[. - 1].e < $m[.].e and $m[. - 1].f > $m[.].f)] | length == 0)
        and ([$r[] | select(.n != "") | .e] | unique | length >= 2)' "$WORK/usage.json"
}
counted_once || fail "counted twice, or under one epoch: $(korpis get usage --workload "$W")"
korpis get usage --workload "$W" >&2

log "the hold released: the node it left deletes its copy"
until_ok 60 korpis migration release "$MIGRATION" --yes
completed() {
    korpis --json migration get "$W" "$MIGRATION" \
        | jq -e '.outcome == "MIGRATION_OUTCOME_COMPLETED"'
}
until_ok 300 completed || fail "never completed: $(korpis migration get "$W" "$MIGRATION")"
until_ok 120 test ! -e "$VOLUME" || fail "$HERE kept $VOLUME"

log "all of it deleted: each node removes what it made"
# Deleting data is irreversible, so a plan that does waits for a person, here the operator.
approve() {
    if [ "$(jq -r .status <<<"$1")" = PLAN_STATUS_PROPOSED ]; then
        korpis plan approve "$(jq -r .id <<<"$1")" >/dev/null
    fi
}
approve "$(korpis --json workload delete "$W" --delete-volumes data,stream_console)"
approve "$(korpis --json extension uninstall "$INSTALLATION" | jq .plan)"
gone() {
    for w in "${WORKLOADS[@]}"; do
        [ -z "$(ctr -n korpis containers ls -q "labels.\"korpis.workload\"==$w")" ] || return 1
        for m in a b; do
            [ -z "$(find "${VOLUMES_OF[$m]}" -maxdepth 1 -name "$w.data")" ] || return 1
        done
    done
    for m in a b; do [ -z "$(ls -A "${NODE_DIR[$m]}/netns")" ] || return 1; done
}
until_ok 180 gone || fail "a node left containers, volumes, or networks"

log "Phase 3 done-when holds"
