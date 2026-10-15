#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The Phase 0 done-when (§4 of docs/20-roadmap.md), end to end, the way an operator runs it: the
# five binaries started as processes with their own flags, the recipe published with oras, and
# every step taken through the `korpis` CLI. It installs a control plane into a fresh database,
# enrolls this machine as a node, applies a thin recipe, waits for it to converge, restarts the
# agent and checks the workload kept running in the same process, has a reader try a change it
# may not make, and then checks the effect log: every permitted action names its grant, and the
# denial is recorded with its subject and the action that was missing.
#
# Needs root, containerd, docker (for scripts/dev-pg.sh and scripts/dev-registry.sh), psql, jq,
# and oras from scripts/tools.sh. Leaves nothing behind: its containers, database, and files are
# removed on exit however it exits.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD

log() { printf '\n== %s\n' "$*" >&2; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" = 0 ] || fail "needs root: the helper and containerd"
[ -S /run/containerd/containerd.sock ] || fail "needs containerd at /run/containerd"
for tool in docker psql jq ctr; do command -v "$tool" >/dev/null || fail "needs $tool"; done
ORAS=$REPO/.tools/bin/oras
[ -x "$ORAS" ] || fail "needs oras: run scripts/tools.sh"

log "building"
cargo build --quiet -p korpis-server -p korpis-agent -p korpis-driver-oci -p korpis-helper \
    -p korpis-cli -p korpis-stream
BIN=$REPO/target/debug

WORK=$(mktemp -d /tmp/korpis-e2e.XXXXXX)
PG=$(scripts/dev-pg.sh up)
REGISTRY=$(scripts/dev-registry.sh up)
DB="korpis_e2e_$$"
PIDS=()
WORKLOAD=""

cleanup() {
    set +e
    for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null; done
    wait 2>/dev/null
    if [ -n "$WORKLOAD" ]; then
        filter="labels.\"korpis.workload\"==$WORKLOAD"
        for id in $(ctr -n korpis containers ls -q "$filter" 2>/dev/null); do
            ctr -n korpis tasks kill -s SIGKILL "$id" >/dev/null 2>&1
            sleep 0.3
            ctr -n korpis tasks rm -f "$id" >/dev/null 2>&1
            ctr -n korpis containers rm "$id" >/dev/null 2>&1
            ctr -n korpis snapshots rm "$id" >/dev/null 2>&1
        done
    fi
    psql "$PG" -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" >/dev/null 2>&1
    rm -rf "$WORK"
}
trap cleanup EXIT

# Waits up to $1 seconds for the command after it to succeed.
until_ok() {
    local secs=$1; shift
    for _ in $(seq 1 $((secs * 2))); do
        if "$@" >/dev/null 2>&1; then return 0; fi
        sleep 0.5
    done
    "$@"
}

# Starts the command after $1 in the background, logging to $WORK/$1.log; its pid is $LAST.
start() {
    local name=$1; shift
    "$@" >>"$WORK/$name.log" 2>&1 &
    LAST=$!
    PIDS+=("$LAST")
}

log "installing the control plane"
psql "$PG" -qc "CREATE DATABASE $DB TEMPLATE template0" >/dev/null
DB_URL="${PG%/*}/$DB"
"$BIN/korpis-server" install --data-dir "$WORK/server" --database-owner-url "$DB_URL" \
    --organization acme >"$WORK/install.txt"
ROOT_TOKEN=$(grep -o 'korpis_[A-Za-z0-9_-]*' "$WORK/install.txt" | head -n1)
[ -n "$ROOT_TOKEN" ] || fail "install printed no root token"
PORT=$((20000 + $$ % 20000))
start server "$BIN/korpis-server" serve --data-dir "$WORK/server" --database-url "$DB_URL" \
    --listen "127.0.0.1:$PORT" --plain-http-registry "$REGISTRY"
export KORPIS_SERVER="https://127.0.0.1:$PORT" KORPIS_CA="$WORK/server/tls/ca.pem"
export KORPIS_TOKEN=$ROOT_TOKEN
korpis() { "$BIN/korpis" --plain-http "$REGISTRY" "$@"; }
until_ok 30 korpis version

log "an organization that runs unsigned recipes, and a project"
ORG=$(korpis --json get organizations | jq -r '.[0].id')
korpis call OrganizationService/SetTrustPolicy \
    -d "{\"organization\": \"$ORG\", \"policy\": {\"allowUnsigned\": true}}" >/dev/null
korpis call ProjectService/CreateProject -d "{\"organization\": \"$ORG\", \"name\": \"games\"}" \
    >/dev/null

log "publishing the thin recipe with oras"
RECIPE="$REGISTRY/e2e/sleeper-$$"
mkdir -p "$WORK/recipe"
cat >"$WORK/recipe/recipe.yaml" <<'EOF'
apiVersion: korpis.dev/v1
kind: Recipe
metadata: {name: sleeper, version: "1.0.0", description: sleeps, license: MIT}
runtime:
  targets:
    - {tier: container, arch: [amd64, arm64], image: "@REGISTRY@/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"}
  entry:
    command: [/bin/sh, -c, "echo $GREETING > /data/greeting; date +%s%N >> /data/starts; exec sleep 3600"]
preset:
  lifecycle: persistent
  console: none
  volumes:
    - {name: data, path: /data, size_default: 64Mi}
  resources:
    memory: {min: 64Mi, recommended: 128Mi}
    cpu:    {min: 100m, recommended: 250m}
config:
  fields:
    - {key: greeting, type: string, default: hello, deliver: env, deliver_as: GREETING}
EOF
# busybox as scripts/dev-registry.sh mirrors it: the run pulls nothing from a public registry.
sed -i "s|@REGISTRY@|$REGISTRY|" "$WORK/recipe/recipe.yaml"
(cd "$WORK/recipe" && "$ORAS" push --plain-http --disable-path-validation \
    --artifact-type application/vnd.korpis.recipe.v1 "$RECIPE:1.0.0" \
    recipe.yaml:application/vnd.korpis.recipe.document.v1+yaml >/dev/null)

log "enrolling this machine as a node"
NODE="$WORK/node"
mkdir -p "$NODE"
JOIN=$(korpis --json node token create "e2e-$$")
"$BIN/korpis-agent" --state-dir "$NODE/agent" join --server "$KORPIS_SERVER" \
    --token "$(jq -r .token <<<"$JOIN")" --ca-fingerprint "$(jq -r .caFingerprint <<<"$JOIN")" \
    --hostname "e2e-$$" >/dev/null
start helper "$BIN/korpis-helper" --socket "$NODE/helper.sock" --volume-root "$NODE/volumes" \
    --state "$NODE/helper"
start driver "$BIN/korpis-driver-oci" --socket "$NODE/driver.sock" \
    --content "$NODE/agent/content" --volume-root "$NODE/volumes" \
    --identity-root "$NODE/agent/identity" --state-dir "$NODE/driver"
until_ok 10 test -S "$NODE/helper.sock"
until_ok 10 test -S "$NODE/driver.sock"
agent() {
    start agent "$BIN/korpis-agent" --state-dir "$NODE/agent" run \
        --driver-socket "$NODE/driver.sock" --helper-socket "$NODE/helper.sock" \
        --plain-http "$REGISTRY"
    AGENT=$LAST
}
agent

log "korpis apply -f: placed on the node and converged"
cat >"$WORK/sleeper.yaml" <<EOF
workload: sleeper
project: games
recipe: $RECIPE:1.0.0
config:
  greeting: hello from e2e
EOF
korpis apply -f "$WORK/sleeper.yaml"
WORKLOAD=$(korpis --json get workloads games | jq -r '.[0].workload.id')
converged() {
    korpis --json status sleeper | jq -e '.convergence == "CONVERGENCE_CONVERGED"'
}
until_ok 180 converged || fail "never converged; agent log: $(tail -n 20 "$WORK/agent.log")"
RUNTIME=$(korpis --json status sleeper | jq -r '.observation.runtimeId')
pid_of() { ctr -n korpis tasks ls | awk -v id="$1" '$1 == id { print $2 }'; }
PID=$(pid_of "$RUNTIME")
[ -n "$PID" ] && [ "$PID" -gt 0 ] || fail "no task for $RUNTIME"
STARTS=$(find "$NODE/volumes" -name starts | head -n1)
until_ok 10 test -s "$STARTS"
grep -qx 'hello from e2e' "$(dirname "$STARTS")/greeting" || fail "the configured value"
echo "running as pid $PID in $RUNTIME" >&2

log "the agent restarts; the workload does not"
kill "$AGENT"
wait "$AGENT" 2>/dev/null || true
agent
adopted() { korpis --json get effects --workload sleeper | jq -e 'any(.[]; .action == "node.adopt")'; }
until_ok 60 adopted || fail "the restarted agent never adopted the workload"
until_ok 60 converged || fail "not converged after the restart"
sleep 2
[ "$(pid_of "$RUNTIME")" = "$PID" ] || fail "the workload's process changed"
[ "$(wc -l <"$STARTS")" = 1 ] || fail "the workload started again"
echo "still pid $PID, started once" >&2

log "a reader tries to stop it, and is denied"
READER=$(korpis --json grant issue --actions workload.read --scope project:games \
    --label "e2e reader" --expires 1h | jq -r .token)
set +e
KORPIS_TOKEN=$READER korpis workload stop sleeper
code=$?
set -e
[ "$code" = 12 ] || fail "a reader stopping a workload exited $code, not 12 (DENIED)"

log "the effect log"
EFFECTS="$WORK/effects.json"
korpis --json get effects --workload sleeper >"$EFFECTS"
jq -r '.[] | [.at, .action, .outcome, (.grant // "-")] | @tsv' "$EFFECTS" >&2
unnamed=$(jq '[.[] | select(.outcome != "EFFECT_OUTCOME_DENIED" and .grant == null)] | length' \
    "$EFFECTS")
[ "$unnamed" = 0 ] || fail "$unnamed permitted actions name no grant"
jq -e 'any(.[]; .outcome == "EFFECT_OUTCOME_DENIED" and .actor.token != null)' "$EFFECTS" \
    >/dev/null || fail "the reader's denial is not in the log"
for action in workload.create plan.approve node.adopt; do
    jq -e --arg a "$action" 'any(.[]; .action == $a)' "$EFFECTS" >/dev/null \
        || fail "no $action effect"
done

log "Phase 0 done-when holds"
