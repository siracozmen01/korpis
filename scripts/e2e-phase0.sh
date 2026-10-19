#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The Phase 0 done-when (§4 of docs/20-roadmap.md), end to end, the way an operator runs it: the
# five binaries started as processes with their own flags, the recipe published with oras, and
# every step taken through the `korpis` CLI. It installs a control plane into a fresh database,
# enrolls this machine as a node, applies a thin recipe, waits for it to converge, upgrades the
# agent (its binary replaced, as a package does, and restarted) and checks the workload kept
# running in the same process, has a reader try a change it may not make, and then checks the
# effect log: every permitted action names its grant, and the denial is recorded with its
# subject and the action that was missing.
#
# Needs root, containerd, docker (for scripts/dev-pg.sh and scripts/dev-registry.sh), psql, jq,
# and oras from scripts/tools.sh. Leaves nothing behind: its processes, containers, networks,
# database, and files are removed on exit however it exits (scripts/e2e-lib.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
# shellcheck source=scripts/e2e-lib.sh
. scripts/e2e-lib.sh
needs ctr

log "building"
cargo build --quiet -p korpis-server -p korpis-agent -p korpis-driver-oci -p korpis-helper \
    -p korpis-cli -p korpis-stream -p korpis-backup -p korpis-install -p korpis-files
begin

log "installing the control plane, an organization that runs unsigned recipes, and a project"
control_plane

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
enroll "e2e-$$"
# The agent runs from a copy installed as a package installs it, so that its upgrade below can
# replace the file under a running node.
INSTALLED="$WORK/installed/korpis-agent"
mkdir -p "$(dirname "$INSTALLED")"
cp "$BIN/korpis-agent" "$INSTALLED"
agent() {
    start agent "$INSTALLED" --state-dir "$NODE/agent" run \
        --driver-socket "$NODE/driver.sock" --helper-socket "$NODE/helper.sock" \
        --plain-http "$REGISTRY" --ingress false
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
WORKLOADS+=("$WORKLOAD")
converged() {
    korpis --json status sleeper | jq -e '.convergence == "CONVERGENCE_CONVERGED"'
}
until_ok 180 converged || fail "never converged; agent log: $(tail -n 20 "$WORK/agent.log")"
RUNTIME=$(korpis --json status sleeper | jq -r '.observation.runtimeId')
pid_of() { ctr -n korpis tasks ls | awk -v id="$1" '$1 == id { print $2 }'; }
PID=$(pid_of "$RUNTIME")
[[ "$PID" =~ ^[1-9][0-9]*$ ]] || fail "no task for $RUNTIME"
STARTS=$(find "$NODE/volumes" -name starts | head -n1)
until_ok 10 test -s "$STARTS"
grep -qx 'hello from e2e' "$(dirname "$STARTS")/greeting" || fail "the configured value"
echo "running as pid $PID in $RUNTIME" >&2

log "the agent is upgraded: its binary replaced and restarted; the workload is not"
kill "$AGENT"
wait "$AGENT" 2>/dev/null || true
BEFORE=$(stat -c %i "$INSTALLED")
cp "$BIN/korpis-agent" "$INSTALLED.new"
mv -f "$INSTALLED.new" "$INSTALLED"
[ "$(stat -c %i "$INSTALLED")" != "$BEFORE" ] || fail "the binary was not replaced"
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
