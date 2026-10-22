#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The Phase 2 done-when (§4 of docs/20-roadmap.md), end to end, the way an operator, its
# resellers, and a customer's staff run it, every step through the `korpis` CLI or, for a person
# signing in, through the calls the panel makes. A control plane is installed into a fresh
# database and this machine enrolled as its node. The operator makes Northwind, a reseller, gives
# it a quota set, and issues it a credential of its own; Northwind, acting with that credential
# and never root's, makes Kestrel beneath it and gives it a smaller set and a credential; Kestrel
# makes Pinecone, its customer, and gives it a set and its administrator's credential. Quota
# inherits: Pinecone's server is placed within all three sets and runs; its next, which fits
# Pinecone's set, is refused by Northwind's, and Pinecone is told no more than that a set it may
# not read refused it, while Northwind, planning the same declaration, is told its own set and
# numbers; and Northwind's own servers cannot take what it guaranteed Kestrel (trace 52 of
# docs/23-walkthroughs.md). Each credential reaches only beneath the organization it was issued
# at. Then an external role grants a person authority over one project with no account made
# anywhere (trace 44): Pinecone installs korpis-idp-oidc against an independent OpenID Connect
# issuer, binds its `ops` group to an Operator template granted on one project, and Dana, whom
# the issuer signs in, restarts a server there and reads nothing else; removed from `ops` at the
# issuer, she loses that authority within the binding's interval. Then it deletes all of it, and
# checks the node removed what it made.
#
# Needs root, containerd, docker (for scripts/dev-pg.sh and scripts/dev-registry.sh), psql, jq,
# curl, openssl, unshare, oras from scripts/tools.sh, and Node.js with conformance/oidc's packages
# installed (pnpm install there). Leaves nothing behind: its processes, containers, networks,
# database, and files are removed on exit however it exits.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
# shellcheck source=scripts/e2e-lib.sh
. scripts/e2e-lib.sh
needs ip curl openssl unshare node
[ -d conformance/oidc/node_modules/oidc-provider ] \
    || fail "needs the issuer's packages: pnpm install in conformance/oidc"

log "building"
cargo build --quiet --locked -p korpis-server -p korpis-agent -p korpis-driver-oci \
    -p korpis-helper -p korpis-cli -p korpis-stream -p korpis-files -p korpis-idp-oidc
begin

log "installing the control plane, told the panel's address a sign-in returns to"
PANEL=1
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
ADMIN=$ADMIN,workload.read,workload.create,workload.write,workload.delete,workload.restart
ADMIN=$ADMIN,plan.read,plan.approve,extension.read,extension.install,template.write
ADMIN=$ADMIN,identity.binding.write
# A new token holding $2 over organization $1, passing all of it on $3 levels further; its secret.
credential() {
    local token=$1; shift
    as "$token" --json grant issue --scope "org:$1" --actions "$2" --delegate "$2" \
        --depth "$3" --label "$1 administration" | jq -er .token
}

log "the operator makes Northwind, a reseller: 512 MiB reserved, 768 sold, and it may oversell"
korpis organization create northwind --parent acme >/dev/null
korpis quota set northwind -f "$(quota_file 512 768 200 northwind)" >/dev/null
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

log "Kestrel, as itself, makes Pinecone, its customer, and gives it all of its own"
as "$KESTREL" organization create pinecone --parent kestrel >/dev/null
as "$KESTREL" quota set pinecone -f "$(quota_file 256 768 100 pinecone)" >/dev/null
PINECONE_ADMIN=$(credential "$KESTREL" pinecone "$ADMIN" 1)
PINECONE_ID=$(as "$PINECONE_ADMIN" --json get organizations | jq -r '.[0].id')
log "Pinecone's administrator makes a project and a token its servers are deployed with"
as "$PINECONE_ADMIN" project create shop --organization pinecone >/dev/null
CUSTOMER=org.read,project.read,workload.read,workload.create,workload.write,plan.read
PINECONE=$(as "$PINECONE_ADMIN" --json grant issue --scope org:pinecone --actions "$CUSTOMER" \
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

log "Northwind runs a support server of its own: 64 MiB reserved, 512 sold to itself"
as "$NORTHWIND" project create support --organization northwind >/dev/null
as "$NORTHWIND" apply -f "$(server support-1 support 64 512)"
WORKLOADS+=("$(id_of support-1)")

log "Pinecone's server is placed within all three sets, and runs"
as "$PINECONE" apply -f "$(server web-1 shop 64 128)"
WEB=$(id_of web-1)
WORKLOADS+=("$WEB")
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
if as "$PINECONE" apply -f "$(server web-2 shop 64 256)" >"$WORK/web-2.txt" 2>&1; then
    fail "Pinecone's second server was admitted past Northwind's limits"
fi
grep -q 'a quota set you may not read bounds memory_bytes limits' "$WORK/web-2.txt" \
    || fail "Pinecone was told: $(cat "$WORK/web-2.txt")"
NORTHWIND_ID=$(as "$NORTHWIND" --json get organizations \
    | jq -r '.[] | select(.name == "northwind") | .id')
if grep -q -e "$NORTHWIND_ID" -e $((768 << 20)) "$WORK/web-2.txt"; then
    fail "Pinecone was told Northwind's set: $(cat "$WORK/web-2.txt")"
fi
if as "$NORTHWIND" plan -f "$WORK/web-2.yaml" >"$WORK/web-2-northwind.txt" 2>&1; then
    fail "Northwind's plan of the same declaration admitted it"
fi
TOLD="the quota set of organization $NORTHWIND_ID bounds memory_bytes limits at $((768 << 20))"
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

log "an independent OpenID Connect issuer, on this machine, that knows Dana, in ops and oncall"
ISSUER_DIR="$WORK/issuer"
mkdir -p "$ISSUER_DIR"
(
    cd "$ISSUER_DIR"
    curve=(-newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes)
    openssl req -x509 "${curve[@]}" -days 1 -subj /CN=korpis-e2e-issuer-authority \
        -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign \
        -keyout ca.key -out ca.pem 2>/dev/null
    openssl req "${curve[@]}" -subj /CN=127.0.0.1 -keyout key.pem -out leaf.csr 2>/dev/null
    printf 'subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' >leaf.ext
    openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 1 \
        -extfile leaf.ext -out cert.pem 2>/dev/null
)
people() { printf '{"next": "dana", "people": {"dana": [%s]}}\n' "$1" >"$ISSUER_DIR/people.json"; }
people '"ops", "oncall"'
SECRET=$(head -c 24 /dev/urandom | base64 | tr -d '/+=')
ISSUER="https://127.0.0.1:$((40000 + $$ % 20000))"
start issuer env PORT="${ISSUER##*:}" PEOPLE="$ISSUER_DIR/people.json" CLIENT_ID=korpis \
    CLIENT_SECRET="$SECRET" REDIRECT_URI="$PANEL_URL/sign-in/callback" \
    TLS_CERT="$ISSUER_DIR/cert.pem" TLS_KEY="$ISSUER_DIR/key.pem" \
    node "$REPO/conformance/oidc/issuer.mjs"
until_ok 30 curl -sf --cacert "$ISSUER_DIR/ca.pem" "$ISSUER/.well-known/openid-configuration" \
    || fail "the issuer never started: $(tail -n 20 "$WORK/issuer.log")"

log "Pinecone's administrator installs korpis-idp-oidc into Pinecone as corp"
mkdir -p "$WORK/corp"
cat >"$WORK/corp/extension.yaml" <<EOF
apiVersion: korpis.dev/v1
kind: Extension
metadata:
  {name: corp-idp, version: "1.0.0", description: signs staff in, license: AGPL-3.0-or-later}
runtime:
  targets:
    - {tier: container, arch: [amd64, arm64], image: "$REGISTRY/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"}
  entry:
    command: [/bin/sh, -c, "trap 'exit 0' TERM; while true; do sleep 1; done"]
preset:
  lifecycle: persistent
  console: none
  resources:
    memory: {min: 32Mi, recommended: 64Mi}
    cpu: {min: 50m, recommended: 100m}
extension:
  api_version: korpis.extension.v1
  requires: [provider.serve, provider.identity]
  provides:
    - {type: identity, name: corp}
EOF
EXTENSION="$REGISTRY/e2e/corp-idp-$$"
(cd "$WORK/corp" && "$ORAS" push --plain-http \
    --artifact-type application/vnd.korpis.extension.v1 "$EXTENSION:1.0.0" \
    extension.yaml:application/vnd.korpis.extension.document.v1+yaml >/dev/null)
as "$PINECONE_ADMIN" project create identity --organization pinecone >/dev/null
DIGEST=$(as "$PINECONE_ADMIN" --json extension install "$EXTENSION" --version 1.0.0 \
    --workload-project identity | jq -r .artifact.digest)
as "$PINECONE_ADMIN" --json extension install "$EXTENSION" --version "$DIGEST" --approve \
    --workload-project identity >"$WORK/corp/installed.json"
INSTALLATION=$(jq -r .extension.id "$WORK/corp/installed.json")
CORP=$(jq -r .extension.workload "$WORK/corp/installed.json")
WORKLOADS+=("$CORP")
# Its workload holds its place on the node, and the provider's own program is run here, through
# the identity socket and the relay the node serves that workload, mounted where the workload's
# own mount puts them: the node's egress rules deny a workload every address of its own host
# (§7 of docs/07-networking.md), and the issuer is on this one.
SOCKETS="$NODE/agent/identity/$CORP"
# Where it keeps the refresh tokens it re-verifies people with: its own volume, in a workload.
install -d -m 0700 "$WORK/corp/state"
until_ok 180 test -S "$SOCKETS/identity.sock" \
    || fail "no identity socket for the provider; agent log: $(tail -n 20 "$WORK/agent.log")"
# shellcheck disable=SC2016 # expanded by the shell it starts, in the mount namespace
start corp unshare --mount sh -c \
    'mount -t tmpfs korpis-e2e /run && mkdir /run/korpis && mount --bind "$0" /run/korpis &&
     exec "$@"' "$SOCKETS" env SSL_CERT_FILE="$ISSUER_DIR/ca.pem" KORPIS_OIDC_ISSUER="$ISSUER" \
    KORPIS_OIDC_CLIENT_ID=korpis KORPIS_OIDC_CLIENT_SECRET="$SECRET" KORPIS_OIDC_SCOPES=groups \
    KORPIS_OIDC_OFFLINE=true KORPIS_OIDC_STATE_DIR="$WORK/corp/state" \
    KORPIS_PROVIDER_NAME=corp RUST_LOG=info "$BIN/korpis-idp-oidc"

log "Pinecone binds its ops group to an Operator template, granted on the shop project alone"
cat >"$WORK/corp/operator.json" <<EOF
{"name": "operator", "actions": ["project.read", "workload.read", "workload.restart"],
 "scopePattern": {"kind": "OBJECT_KIND_PROJECT"}, "delegation": {"allowed": false}}
EOF
TEMPLATE=$(as "$PINECONE_ADMIN" --json template create pinecone -f "$WORK/corp/operator.json" \
    | jq -er '[.. | objects | select(has("actions") and has("version"))][0]
              | "\(.id)@\(.version)"')
OPS=$(as "$PINECONE_ADMIN" --json identity bind set:groups/ops --provider corp --org pinecone \
    --template "$TEMPLATE" --interval 1m | jq -er 'first(.. | objects | select(has("subject"))).id')
# Unverified until its provider first answers for it, within seconds; until then nothing rooted
# in it is in force.
bound() {
    as "$PINECONE_ADMIN" --json identity show "$OPS" \
        | jq -e '[.. | objects | select(has("subject"))][0].state == "BINDING_STATE_VERIFIED"'
}
until_ok 60 bound || fail "the binding was never verified: $(tail -n 20 "$WORK/corp.log")"
as "$PINECONE_ADMIN" grant issue --template operator --scope project:shop \
    --to "ext:$PINECONE_ID/corp:set:groups:ops" --label "pinecone's operators" >/dev/null

log "Dana signs in at Pinecone's link, the panel's part played by curl, PKCE and all"
accounts() { psql "${PG%/*}/$DB" -tAc 'SELECT count(*) FROM account'; }
ACCOUNTS=$(accounts)
VERIFIER=$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=')
CHALLENGE=$(printf %s "$VERIFIER" | openssl dgst -sha256 -binary | base64 | tr '+/' '-_' \
    | tr -d '=')
# A call of the session service as the panel makes it, its cookies kept.
panel() {
    curl -sS --fail-with-body --cacert "$KORPIS_CA" -b "$WORK/panel.jar" -c "$WORK/panel.jar" \
        -H 'Content-Type: application/json' -H "Origin: $PANEL_URL" \
        "$KORPIS_SERVER/korpis.v1.SessionService/$1" -d "$2"
}
begun() {
    panel BeginExternalSignIn "{\"organization\": \"$PINECONE_ID\", \"provider\": \"corp\",
        \"codeChallenge\": \"$CHALLENGE\"}" >"$WORK/begun.json"
}
until_ok 120 begun || fail "the sign-in never began: $(cat "$WORK/begun.json")"
AT=$(jq -er .url "$WORK/begun.json")
case "$AT" in "$ISSUER"/*) ;; *) fail "sent elsewhere than the issuer: $AT" ;; esac
# Dana's browser at the issuer: redirects followed, with its cookies, until it is sent back.
for _ in $(seq 1 12); do
    case "$AT" in "$PANEL_URL/sign-in/callback"*) break ;; esac
    AT=$(curl -sS --cacert "$ISSUER_DIR/ca.pem" -b "$WORK/issuer.jar" -c "$WORK/issuer.jar" \
        -o /dev/null -w '%{redirect_url}' "$AT")
    [ -n "$AT" ] || fail "the issuer stopped sending Dana anywhere"
done
case "$AT" in "$PANEL_URL/sign-in/callback"*) ;; *) fail "never sent back: $AT" ;; esac
# The parameter $1 of the address Dana was sent back to, decoded.
query() {
    local v
    v=$(sed -n "s/.*[?&]$1=\([^&#]*\).*/\1/p" <<<"$AT")
    printf '%b' "${v//%/\\x}"
}
panel CompleteExternalSignIn "{\"state\": \"$(query state)\", \"code\": \"$(query code)\",
    \"codeVerifier\": \"$VERIFIER\"}" >"$WORK/signed-in.json"
# Her access token, renewed from the session's refresh cookie as the panel renews it: each lasts
# 120 seconds, and the session stands while her authority comes and goes.
dana() { panel Exchange '{}' | jq -er .accessToken; }
DANA=$(dana)
[[ $DANA == korpisax_* ]] || fail "Dana holds no person's session"

log "Dana restarts the shop's server, reads nothing else, and no account was made for her"
as "$DANA" status web-1 >/dev/null || fail "Dana cannot read the shop's server"
as "$DANA" workload restart web-1 >/dev/null || fail "Dana cannot restart the shop's server"
for w in "$CORP" "$(id_of support-1)"; do
    if as "$DANA" status "$w" >"$WORK/elsewhere.txt" 2>&1; then fail "Dana read $w"; fi
    grep -q NOT_FOUND "$WORK/elsewhere.txt" || fail "Dana was told: $(cat "$WORK/elsewhere.txt")"
done
[ "$(accounts)" = "$ACCOUNTS" ] || fail "an account was made: $ACCOUNTS before, $(accounts) now"

log "Dana leaves ops at the issuer: her authority ends within the binding's interval"
people '"oncall"'
# Refused as not there to read, not as signed out: her session stands, her authority does not.
ended() {
    ! as "$(dana)" status "$WEB" >"$WORK/ended.txt" 2>&1 && grep -q NOT_FOUND "$WORK/ended.txt"
}
# Asked every five seconds, as a person would, well within what her rate limit allows.
for _ in $(seq 1 48); do
    if ended; then break; fi
    sleep 5
done
ended || fail "Dana still acts, or was told: $(cat "$WORK/ended.txt"); the provider said:
$(tail -n 20 "$WORK/corp.log")"
[ "$(accounts)" = "$ACCOUNTS" ] || fail "an account was made after all"

log "all of it deleted: the node removes what it made"
approve() {
    if [ "$(jq -r .status <<<"$1")" = PLAN_STATUS_PROPOSED ]; then
        korpis plan approve "$(jq -r .id <<<"$1")" >/dev/null
    fi
}
for w in web-1 support-1; do
    approve "$(korpis --json workload delete "$w")"
done
approve "$(korpis --json extension uninstall "$INSTALLATION" | jq .plan)"
gone() {
    for w in "${WORKLOADS[@]}"; do
        [ -z "$(ctr -n korpis containers ls -q "labels.\"korpis.workload\"==$w")" ] || return 1
    done
    [ -z "$(ls -A "$NODE/netns")" ]
}
until_ok 180 gone || fail "the node left containers or networks: $(ls "$NODE/netns")"

log "Phase 2 done-when holds"
