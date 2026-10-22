#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The Phase 1 done-when (§4 of docs/20-roadmap.md), end to end, the way an operator runs it: the
# single-machine installation of §1 of docs/18-operations.md made real, with its processes started
# with their own flags, its artifacts published with oras, and every step through the `korpis`
# CLI. It installs a control plane into a fresh database and enrolls this machine as its node,
# its volumes on a btrfs with qgroups on; installs the backup target for a disk as an extension,
# from its static binary pushed as an image; runs a workload with a 64 MiB volume and an
# interactive console, reads the console and types into it, has the kernel refuse a write past
# the volume's quota and checks the memory limit its status shows is its cgroup's; backs its
# volume up, reads a file back out of the backup, and restores the backup into a clone whose
# volume holds that file; then deletes all of it, and checks the node removed what it made.
#
# Needs root, containerd, docker (for scripts/dev-pg.sh and scripts/dev-registry.sh), psql, jq,
# mkfs.btrfs and btrfs (btrfs-progs), and oras from scripts/tools.sh. Leaves nothing behind: its
# processes, containers, networks, filesystem, database, and files are removed on exit however it
# exits.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
# shellcheck source=scripts/e2e-lib.sh
. scripts/e2e-lib.sh
needs mkfs.btrfs btrfs ip sha256sum

log "building"
cargo build --quiet --locked -p korpis-server -p korpis-agent -p korpis-driver-oci \
    -p korpis-helper -p korpis-cli -p korpis-stream -p korpis-backup -p korpis-install \
    -p korpis-files
# The backup target runs in a container that holds nothing but itself, so it is linked static,
# into a target directory of its own, as the end-to-end tests build it.
TRIPLE="$(uname -m)-unknown-linux-gnu"
CARGO_ENCODED_RUSTFLAGS=$'-C\x1ftarget-feature=+crt-static\x1f-C\x1fdebuginfo=0\x1f-C\x1fstrip=symbols' \
    RUSTFLAGS='' cargo build --quiet --locked -p korpis-target-fs --bin korpis-target-fs \
    --target "$TRIPLE" --target-dir target/static
TARGET_BINARY=$REPO/target/static/$TRIPLE/debug/korpis-target-fs
begin

log "installing the control plane"
control_plane

log "publishing the backup target's image and its extension with oras"
case "$(uname -m)" in aarch64) ARCH=arm64 ;; *) ARCH=amd64 ;; esac
mkdir -p "$WORK/image/root"
install -m 0755 "$TARGET_BINARY" "$WORK/image/root/korpis-target-fs"
tar --owner=0 --group=0 --numeric-owner -C "$WORK/image/root" -cf "$WORK/image/layer.tar" \
    korpis-target-fs
cat >"$WORK/image/config.json" <<EOF
{"architecture": "$ARCH", "os": "linux", "config": {"Entrypoint": ["/korpis-target-fs"]},
 "rootfs": {"type": "layers",
            "diff_ids": ["sha256:$(sha256sum "$WORK/image/layer.tar" | cut -d' ' -f1)"]}}
EOF
IMAGE_REPO="$REGISTRY/e2e/korpis-target-fs-$$"
IMAGE_DIGEST=$(cd "$WORK/image" && "$ORAS" push --plain-http --format json \
    --config config.json:application/vnd.oci.image.config.v1+json "$IMAGE_REPO:1.0.0" \
    layer.tar:application/vnd.oci.image.layer.v1.tar | jq -r .digest)
mkdir -p "$WORK/extension"
cat >"$WORK/extension/extension.yaml" <<EOF
apiVersion: korpis.dev/v1
kind: Extension
metadata: {name: fs-target, version: "1.0.0", description: backups on a disk, license: AGPL-3.0-or-later}
runtime:
  targets:
    - {tier: container, arch: [amd64, arm64], image: "$IMAGE_REPO@$IMAGE_DIGEST"}
  entry:
    command: [/korpis-target-fs]
preset:
  lifecycle: persistent
  console: none
  volumes:
    - {name: objects, path: /data, size_default: 64Mi}
  endpoints:
    - {name: store, protocol: tcp, port: 9000, exposure_default: node_port}
  resources:
    memory: {min: 64Mi, recommended: 128Mi}
    cpu: {min: 100m, recommended: 250m}
extension:
  api_version: korpis.extension.v1
  requires: [provider.serve, provider.backup_target]
  provides:
    - {type: backup_target, name: fs}
EOF
EXTENSION="$REGISTRY/e2e/fs-target-$$"
(cd "$WORK/extension" && "$ORAS" push --plain-http \
    --artifact-type application/vnd.korpis.extension.v1 "$EXTENSION:1.0.0" \
    extension.yaml:application/vnd.korpis.extension.document.v1+yaml >/dev/null)

log "publishing the workload's recipe: a shell that runs what is typed into its console"
RECIPE="$REGISTRY/e2e/shell-$$"
mkdir -p "$WORK/recipe"
cat >"$WORK/recipe/recipe.yaml" <<EOF
apiVersion: korpis.dev/v1
kind: Recipe
metadata: {name: shell, version: "1.0.0", description: runs what is typed, license: MIT}
runtime:
  targets:
    - {tier: container, arch: [amd64, arm64], image: "$REGISTRY/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"}
  entry:
    command: [/bin/sh, -c, 'echo ready; while read -r line; do sh -c "\$line" 2>&1; echo "exit \$?"; done']
preset:
  lifecycle: persistent
  console: interactive
  volumes:
    - {name: data, path: /data, size_default: 64Mi}
  resources:
    memory: {min: 64Mi, recommended: 128Mi}
    cpu:    {min: 100m, recommended: 250m}
EOF
(cd "$WORK/recipe" && "$ORAS" push --plain-http \
    --artifact-type application/vnd.korpis.recipe.v1 "$RECIPE:1.0.0" \
    recipe.yaml:application/vnd.korpis.recipe.document.v1+yaml >/dev/null)

log "a btrfs for the volumes, with qgroups on"
MOUNTED="$WORK/mnt"
mkdir -p "$MOUNTED"
truncate -s 2G "$WORK/volumes.img"
mkfs.btrfs -q "$WORK/volumes.img"
mount -o loop "$WORK/volumes.img" "$MOUNTED"
btrfs quota enable "$MOUNTED"
btrfs quota rescan -w "$MOUNTED" >/dev/null

log "enrolling this machine as a node, its volumes on that btrfs"
enroll "e2e-$$" "$MOUNTED/volumes"
# Its endpoints reached at this machine's own address, on ports and a pool of this run's own.
HOST=$(ip -4 -o route get 1.1.1.1 | sed -n 's/.* src \([0-9.]*\).*/\1/p')
[ -n "$HOST" ] || fail "this machine has no address towards the internet to publish ports on"
FIRST=$((33000 + $$ % 50 * 20))
start agent "$BIN/korpis-agent" --state-dir "$NODE/agent" run \
    --driver-socket "$NODE/driver.sock" --helper-socket "$NODE/helper.sock" \
    --plain-http "$REGISTRY" --ingress false --advertise-address "$HOST" \
    --ports "$FIRST-$((FIRST + 19))" --workload-ipv4 10.198.0.0/16

log "the backup target installed as an extension, and a repository on it"
korpis --json extension install "$EXTENSION" --version 1.0.0 --workload-project games \
    >"$WORK/review.json"
DIGEST=$(jq -r .artifact.digest "$WORK/review.json")
korpis --json extension install "$EXTENSION" --version "$DIGEST" --approve \
    --workload-project games >"$WORK/installed.json"
INSTALLATION=$(jq -r .extension.id "$WORK/installed.json")
TARGET=$(jq -r .extension.workload "$WORK/installed.json")
WORKLOADS+=("$TARGET")
pinged() {
    korpis call ProviderService/PingProvider -d "{\"organization\": \"$ORG\",
        \"type\": \"PROVIDER_TYPE_BACKUP_TARGET\", \"name\": \"fs\"}" | jq -e 'has("error") | not'
}
until_ok 180 pinged || fail "the target never answered; agent log: $(tail -n 20 "$WORK/agent.log")"
REPOSITORY=$(korpis --json repository create nightly --provider fs --prefix korpis/ | jq -r .id)

log "korpis apply -f: a workload with a 64 MiB volume, an interactive console, a memory limit"
cat >"$WORK/shell.yaml" <<EOF
workload: shell
project: games
recipe: $RECIPE:1.0.0
resources:
  cpu: {reservation: 100m, limit: 250m}
  memory: {reservation: 64Mi, limit: 96Mi}
volumes:
  - {name: data, size: 64Mi}
EOF
korpis apply -f "$WORK/shell.yaml"
# A workload's identifier, by its name in `games`.
id_of() {
    korpis --json get workloads games \
        | jq -r --arg n "$1" '.[] | select(.workload.name == $n) | .workload.id'
}
SHELL_ID=$(id_of shell)
WORKLOADS+=("$SHELL_ID")
converged() { korpis --json status "$1" | jq -e '.convergence == "CONVERGENCE_CONVERGED"'; }
until_ok 180 converged shell || fail "never converged; agent log: $(tail -n 20 "$WORK/agent.log")"

log "its console read, and typed into"
# Each argument is typed as a line; the command ends a moment after its input does.
type_in() { printf '%s\n' "$@" | korpis console shell; }
said() { korpis logs shell | grep -q -- "$1"; }
until_ok 30 said ready || fail "the console never said it was ready: $(korpis logs shell)"
type_in 'echo typed by the operator' >"$WORK/typed.txt"
grep -q 'typed by the operator' "$WORK/typed.txt" \
    || fail "the answer, attached: $(cat "$WORK/typed.txt")"
until_ok 30 said 'typed by the operator' || fail "the answer, in the console's log"

log "the volume's quota, refused by the kernel past 64 MiB"
type_in 'dd if=/dev/zero of=/data/fill bs=1M count=96 conv=fsync' >/dev/null
until_ok 60 said 'Disk quota exceeded' \
    || fail "a write past 64 MiB was not refused: $(korpis logs shell | tail -n 5)"
type_in 'rm /data/fill' >/dev/null

log "its memory limit, as its status shows it, is its cgroup's"
LIMIT=$(korpis --json status shell | jq -r .target.body.resources.memory.limit)
[ "$LIMIT" = $((96 << 20)) ] || fail "its status shows a memory limit of $LIMIT"
RUNTIME=$(korpis --json status shell | jq -r .observation.runtimeId)
PID=$(ctr -n korpis tasks ls | awk -v id="$RUNTIME" '$1 == id { print $2 }')
[[ "$PID" =~ ^[1-9][0-9]*$ ]] || fail "no task for $RUNTIME"
CGROUP=$(sed -n 's/^0:://p' "/proc/$PID/cgroup")
MAX=$(cat "/sys/fs/cgroup$CGROUP/memory.max")
[ "$MAX" = "$LIMIT" ] || fail "its cgroup's memory.max is $MAX, its status says $LIMIT"
echo "memory.max $MAX in $CGROUP" >&2

log "a file written through the relay, and the volume backed up whole"
printf 'the first world\n' >"$WORK/level.dat"
korpis files mkdir w:shell/data/saves
korpis files put "$WORK/level.dat" w:shell/data/saves/level.dat
VOLUME=$(korpis --json get volumes games --workload shell \
    | jq -r '.[] | select(.name == "data") | .id')
korpis backup create "$VOLUME" --repository "$REPOSITORY"
backed_up() {
    korpis --json backup list shell | jq -e '.[0].status == "BACKUP_STATUS_SUCCEEDED"'
}
until_ok 120 backed_up || fail "the backup never succeeded: $(korpis backup list shell)"
korpis backup list shell >&2
BACKUP_ID=$(korpis --json backup list shell | jq -r '.[0].id')

log "a file read back out of the backup"
korpis backup cat "$BACKUP_ID" saves/level.dat >"$WORK/read.dat"
cmp -s "$WORK/level.dat" "$WORK/read.dat" || fail "read back: $(cat "$WORK/read.dat")"

log "the backup restored into a clone, whose volume holds the file"
korpis backup clone "$BACKUP_ID" --name shell-copy
COPY_ID=$(id_of shell-copy)
WORKLOADS+=("$COPY_ID")
until_ok 180 converged shell-copy || fail "the clone never converged"
korpis files get w:shell-copy/data/saves/level.dat "$WORK/restored.dat"
cmp -s "$WORK/level.dat" "$WORK/restored.dat" || fail "restored: $(cat "$WORK/restored.dat")"

log "all of it deleted with its volumes: the node removes what it made"
# Deleting data is irreversible, so a plan that does waits for a person, here the operator.
approve() {
    if [ "$(jq -r .status <<<"$1")" = PLAN_STATUS_PROPOSED ]; then
        korpis plan approve "$(jq -r .id <<<"$1")" >/dev/null
    fi
}
TARGET_VOLUME=$(korpis --json get volumes games --workload "$TARGET" | jq -r '.[0].id')
for w in shell shell-copy; do
    approve "$(korpis --json workload delete "$w" --delete-volumes data,stream_console)"
done
approve "$(korpis --json extension uninstall "$INSTALLATION" | jq .plan)"
retired() { korpis --json status "$TARGET" | jq -e '.workload.deletedAt != null'; }
until_ok 120 retired || fail "the target was never retired: $(korpis status "$TARGET")"
approve "$(korpis --json volume delete "$TARGET_VOLUME")"
gone() {
    for w in "${WORKLOADS[@]}"; do
        [ -z "$(ctr -n korpis containers ls -q "labels.\"korpis.workload\"==$w")" ] || return 1
        [ -z "$(find "$VOLUMES" -maxdepth 1 -name "$w.*")" ] || return 1
    done
    [ -z "$(ls -A "$NODE/netns")" ]
}
until_ok 180 gone || fail "the node left containers, volumes, or networks: $(ls "$VOLUMES" \
    "$NODE/netns")"

log "Phase 1 done-when holds"
