#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The Phase 2 done-when (§4 of docs/20-roadmap.md), end to end, the way an operator and its
# resellers run it, every step through the `korpis` CLI. A control plane is installed into a
# fresh database and this machine enrolled as its node. The operator makes Northwind, a reseller,
# gives it a quota set, and issues it a credential of its own; Northwind, acting with that
# credential and never root's, makes Kestrel beneath it, gives it a smaller set and a credential;
# Kestrel makes Pinecone, its customer, and gives it a set, a project, and a credential. Quota
# inherits: Pinecone's server is placed within all three sets and runs; its next, which fits
# Pinecone's set, is refused by Northwind's, and Pinecone is told no more than that a set it may
# not read refused it, while Northwind, reading the same declaration, is told its own set and
# numbers; and Northwind's own servers cannot take what it guaranteed Kestrel (trace 52 of
# docs/23-walkthroughs.md). Each credential reaches only beneath the organization it was issued
# at. Then it deletes all of it, and checks the node removed what it made.
#
# Needs root, containerd, docker (for scripts/dev-pg.sh and scripts/dev-registry.sh), psql, jq,
# and oras from scripts/tools.sh. Leaves nothing behind: its processes, containers, networks,
# database, and files are removed on exit however it exits.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
# shellcheck source=scripts/e2e-lib.sh
. scripts/e2e-lib.sh
needs ip

log "building"
cargo build --quiet --locked -p korpis-server -p korpis-agent -p korpis-driver-oci \
    -p korpis-helper -p korpis-cli -p korpis-stream -p korpis-files
begin

log "installing the control plane"
control_plane

log "publishing the servers' recipe: a process that waits"
RECIPE="$REGISTRY/e2e/waiter-$$"
mkdir -p "$WORK/recipe"
cat >"$WORK/recipe/recipe.yaml" <<EOF
apiVersion: korpis.dev/v1
kind: Recipe
metadata: {name: waiter, version: "1.0.0", description: waits, license: MIT}
runtime:
  targets:
    - {tier: container, arch: [amd64, arm64], image: "$REGISTRY/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"}
  entry:
    command: [/bin/sh, -c, 'echo ready; exec sleep 2147483647']
preset:
  lifecycle: persistent
  console: none
  resources:
    memory: {min: 32Mi, recommended: 64Mi}
    cpu:    {min: 50m, recommended: 100m}
EOF
(cd "$WORK/recipe" && "$ORAS" push --plain-http \
    --artifact-type application/vnd.korpis.recipe.v1 "$RECIPE:1.0.0" \
    recipe.yaml:application/vnd.korpis.recipe.document.v1+yaml >/dev/null)

log "enrolling this machine as a node"
enroll "e2e-$$"
HOST=$(ip -4 -o route get 1.1.1.1 | sed -n 's/.* src \([0-9.]*\).*/\1/p')
[ -n "$HOST" ] || fail "this machine has no address towards the internet to publish ports on"
FIRST=$((33000 + $$ % 50 * 20))
start agent "$BIN/korpis-agent" --state-dir "$NODE/agent" run \
    --driver-socket "$NODE/driver.sock" --helper-socket "$NODE/helper.sock" \
    --plain-http "$REGISTRY" --ingress false --advertise-address "$HOST" \
    --ports "$FIRST-$((FIRST + 19))" --workload-ipv4 10.199.0.0/16

# The `korpis` command run with the credential $1 rather than root's.
as() {
    local token=$1; shift
    KORPIS_TOKEN=$token "$BIN/korpis" --plain-http "$REGISTRY" "$@"
}
# A quota set of one entry for everything, reserving $1 MiB of memory and limiting it to $2,
# its children's limits permitted to sum to $3 percent of its own; written to $WORK/$4.json.
quota_file() {
    cat >"$WORK/$4.json" <<EOF
{"entries": [{"reservationBounds": {"memoryBytes": "$(($1 << 20))"},
              "limitBounds": {"memoryBytes": "$(($2 << 20))"}}],
 "childOvercommitPercent": {"memory_bytes": $3}}
EOF
    echo "$WORK/$4.json"
}
# What a tenant administrator of one level does, and passes on.
ADMIN=org.create,org.read,org.write,org.quota.write,project.create,project.read,grant.read
ADMIN=$ADMIN,workload.read,workload.create,workload.write,workload.delete,plan.read,plan.approve
# A new token holding $2 over organization $1, passing all of it on $3 levels further; its secret.
credential() {
    local token=$1; shift
    as "$token" --json grant issue --scope "org:$1" --actions "$2" --delegate "$2" \
        --depth "$3" --label "$1 administration" | jq -er .token
}

log "the operator makes Northwind, a reseller, with 512 MiB reserved and sold, and oversells it"
korpis organization create northwind --parent acme >/dev/null
korpis quota set northwind -f "$(quota_file 512 512 200 northwind)" >/dev/null
NORTHWIND=$(credential "$KORPIS_TOKEN" northwind "$ADMIN" 3)

log "Northwind, as itself, makes Kestrel beneath it, gives it 256 MiB and sells it 768"
as "$NORTHWIND" organization create kestrel --parent northwind >/dev/null
as "$NORTHWIND" quota set kestrel -f "$(quota_file 256 768 100 kestrel)" >/dev/null
KESTREL=$(credential "$NORTHWIND" kestrel "$ADMIN" 2)
# Asked by identifier, so the control plane answers rather than the command's own lookup.
if as "$NORTHWIND" organization create beside --parent "$ORG" >"$WORK/beside.txt" 2>&1; then
    fail "Northwind made an organization beside itself"
fi
grep -q NOT_FOUND "$WORK/beside.txt" || fail "the host was seen: $(cat "$WORK/beside.txt")"

log "Kestrel, as itself, makes Pinecone, its customer, gives it all of its own, and a project"
as "$KESTREL" organization create pinecone --parent kestrel >/dev/null
as "$KESTREL" quota set pinecone -f "$(quota_file 256 768 100 pinecone)" >/dev/null
as "$KESTREL" project create shop --organization pinecone >/dev/null
CUSTOMER=org.read,project.read,workload.read,workload.create,workload.write,plan.read
PINECONE=$(as "$KESTREL" --json grant issue --scope org:pinecone --actions "$CUSTOMER" \
    --label "pinecone's servers" | jq -er .token)
if as "$KESTREL" grant issue --scope org:northwind --actions org.read --label wider \
    >"$WORK/wider.txt" 2>&1; then
    fail "Kestrel issued a grant over Northwind"
fi

# A server named $1 in project $2, reserving $3 MiB of memory and limited to $4; its file.
server() {
    cat >"$WORK/$1.yaml" <<EOF
workload: $1
project: $2
recipe: $RECIPE:1.0.0
resources:
  cpu: {reservation: 50m, limit: 100m}
  memory: {reservation: $3Mi, limit: $4Mi}
EOF
    echo "$WORK/$1.yaml"
}
id_of() { korpis --json status "$1" | jq -r .workload.id; }
converged() { korpis --json status "$1" | jq -e '.convergence == "CONVERGENCE_CONVERGED"'; }

log "Northwind runs a support server of its own: 64 MiB reserved, 384 sold to itself"
as "$NORTHWIND" project create support --organization northwind >/dev/null
as "$NORTHWIND" apply -f "$(server support-1 support 64 384)"
WORKLOADS+=("$(id_of support-1)")

log "Pinecone's server is placed within all three sets, and runs"
as "$PINECONE" apply -f "$(server web-1 shop 64 128)"
WORKLOADS+=("$(id_of web-1)")
until_ok 180 converged web-1 || fail "never converged; agent log: $(tail -n 20 "$WORK/agent.log")"
until_ok 180 converged support-1 || fail "Northwind's own server never converged"
held() {
    as "$1" --json quota show "$2" \
        | jq -er '[.. | objects | select(has("held")) | .held.reservations.memoryBytes][0]'
}
for at in "$KESTREL pinecone" "$NORTHWIND kestrel"; do
    read -r token org <<<"$at"
    [ "$(held "$token" "$org")" = $((64 << 20)) ] || fail "$org holds $(held "$token" "$org")"
done

log "its next fits Pinecone's set and not what Northwind has left to sell: refused by Northwind's"
if as "$PINECONE" apply -f "$(server web-2 shop 64 128)" >"$WORK/web-2.txt" 2>&1; then
    fail "Pinecone's second server was admitted past Northwind's limits"
fi
grep -q 'a quota set you may not read bounds memory_bytes limits' "$WORK/web-2.txt" \
    || fail "Pinecone was told: $(cat "$WORK/web-2.txt")"
NORTHWIND_ID=$(as "$NORTHWIND" --json get organizations \
    | jq -r '.[] | select(.name == "northwind") | .id')
if grep -q -e "$NORTHWIND_ID" -e $((512 << 20)) "$WORK/web-2.txt"; then
    fail "Pinecone was told Northwind's set: $(cat "$WORK/web-2.txt")"
fi
if as "$NORTHWIND" plan -f "$WORK/web-2.yaml" >"$WORK/web-2-northwind.txt" 2>&1; then
    fail "Northwind's plan of the same declaration admitted it"
fi
TOLD="the quota set of organization $NORTHWIND_ID bounds memory_bytes limits at $((512 << 20))"
grep -qF "$TOLD" "$WORK/web-2-northwind.txt" \
    || fail "Northwind was told: $(cat "$WORK/web-2-northwind.txt")"

log "Northwind cannot run into what it guaranteed Kestrel"
if as "$NORTHWIND" apply -f "$(server support-2 support 256 256)" >"$WORK/support-2.txt" 2>&1
then
    fail "Northwind's second server took what Kestrel was guaranteed"
fi
grep -q "organization $NORTHWIND_ID bounds memory_bytes reservations" "$WORK/support-2.txt" \
    || fail "Northwind was told: $(cat "$WORK/support-2.txt")"

log "each credential reaches only beneath where it was issued"
names() { as "$1" --json get organizations | jq -r '[.[].name] | sort | join(",")'; }
[ "$(names "$PINECONE")" = pinecone ] || fail "Pinecone reads $(names "$PINECONE")"
[ "$(names "$KESTREL")" = kestrel,pinecone ] || fail "Kestrel reads $(names "$KESTREL")"
[ "$(names "$NORTHWIND")" = kestrel,northwind,pinecone ] \
    || fail "Northwind reads $(names "$NORTHWIND")"
for who in "$PINECONE" "$KESTREL"; do
    if as "$who" status support-1 >/dev/null 2>&1; then
        fail "Northwind's own server was read from beneath it"
    fi
done
as "$KESTREL" status web-1 >/dev/null || fail "Kestrel cannot read its customer's server"
as "$NORTHWIND" status web-1 >/dev/null || fail "Northwind cannot read its customers' server"

log "all of it deleted: the node removes what it made"
approve() {
    if [ "$(jq -r .status <<<"$1")" = PLAN_STATUS_PROPOSED ]; then
        korpis plan approve "$(jq -r .id <<<"$1")" >/dev/null
    fi
}
for w in web-1 support-1; do
    approve "$(korpis --json workload delete "$w")"
done
gone() {
    for w in "${WORKLOADS[@]}"; do
        [ -z "$(ctr -n korpis containers ls -q "labels.\"korpis.workload\"==$w")" ] || return 1
    done
    [ -z "$(ls -A "$NODE/netns")" ]
}
until_ok 180 gone || fail "the node left containers or networks: $(ls "$NODE/netns")"

log "Phase 2 done-when, its tenancy half, holds"
