#!/usr/bin/env bash
# An OCI registry for development and tests, isolated from anything else on the machine: its own
# container, its own named volume, bound to loopback only, plain HTTP. Recipes are OCI artifacts
# (§2 of docs/09-recipes.md); tests push recipes here and the control plane resolves them.
#
#   scripts/dev-registry.sh up       start it (idempotent) and wait until it answers
#   scripts/dev-registry.sh down     stop it, keeping its content
#   scripts/dev-registry.sh destroy  stop it and delete its content
#   scripts/dev-registry.sh url      print its address
set -euo pipefail
NAME=korpis-dev-registry
VOLUME=korpis-dev-registry-data
PORT=${KORPIS_DEV_REGISTRY_PORT:-55000}
IMAGE=${KORPIS_DEV_REGISTRY_IMAGE:-registry:3}
ADDR="127.0.0.1:$PORT"

case "${1:-}" in
up)
    if ! docker container inspect "$NAME" >/dev/null 2>&1; then
        docker run -d --name "$NAME" --restart unless-stopped \
            -p "$ADDR:5000" \
            -v "$VOLUME:/var/lib/registry" \
            "$IMAGE" >/dev/null
    elif [ "$(docker container inspect -f '{{.State.Running}}' "$NAME")" != true ]; then
        docker start "$NAME" >/dev/null
    fi
    for _ in $(seq 1 60); do
        if curl -fsS "http://$ADDR/v2/" >/dev/null 2>&1; then
            echo "$ADDR"; exit 0
        fi
        sleep 0.5
    done
    echo "registry did not become ready" >&2; exit 1
    ;;
down) docker stop "$NAME" >/dev/null ;;
destroy) docker rm -f "$NAME" >/dev/null 2>&1 || true; docker volume rm "$VOLUME" >/dev/null 2>&1 || true ;;
url) echo "$ADDR" ;;
*) sed -n '2,10p' "$0"; exit 2 ;;
esac
