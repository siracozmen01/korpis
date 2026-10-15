#!/usr/bin/env bash
# An OCI registry for development and tests, isolated from anything else on the machine: its own
# container, its own named volume, bound to loopback only, plain HTTP. Recipes are OCI artifacts
# (§2 of docs/09-recipes.md); tests push recipes here and the control plane resolves them. `up`
# also mirrors the images the tests run into it, by digest, so no test pulls from a public
# registry, whose limits on anonymous pulls would make tests fail at random.
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
# The images the tests run, by digest; each is mirrored as library/<name> under the same digest.
MIRRORED=(
    "docker.io/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"
)
INDEX="application/vnd.oci.image.index.v1+json,application/vnd.docker.distribution.manifest.list.v2+json"

# Copies each image of MIRRORED that the registry does not hold yet, once; oras keeps its digest.
mirror() {
    cd "$(dirname "$0")/.."
    for image in "${MIRRORED[@]}"; do
        local digest=${image##*@} name=${image%@*}
        name=${name##*/}
        if curl -fsS -o /dev/null -H "Accept: $INDEX" \
            "http://$ADDR/v2/library/$name/manifests/$digest" 2>/dev/null; then
            continue
        fi
        ./scripts/tools.sh >/dev/null
        .tools/bin/oras cp --to-plain-http "$image" "$ADDR/library/$name:mirror" >/dev/null
    done
}

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
            mirror
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
