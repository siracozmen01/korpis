#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# A PostgreSQL for development and tests, isolated from anything else on the machine: its own
# container, bound to loopback only. Its data is in memory and never synced, since every test makes
# and drops databases of its own and nothing in it is worth keeping: on the shared build machine a
# disk busy with other work held a single statement here for minutes, and with it every test.
#
#   scripts/dev-pg.sh up       start it (idempotent) and wait until it accepts connections
#   scripts/dev-pg.sh down     stop it, losing its data
#   scripts/dev-pg.sh destroy  stop it and remove it
#   scripts/dev-pg.sh url      print the superuser URL (tests create throwaway databases with it)
#   scripts/dev-pg.sh psql     open psql as the superuser
set -euo pipefail
NAME=korpis-dev-postgres
VOLUME=korpis-dev-postgres-data
PORT=${KORPIS_DEV_PG_PORT:-55432}
IMAGE=${KORPIS_DEV_PG_IMAGE:-postgres:17}
PASSWORD=korpis-dev
URL="postgres://postgres:$PASSWORD@127.0.0.1:$PORT/postgres"

case "${1:-}" in
up)
    if ! docker container inspect "$NAME" >/dev/null 2>&1; then
        docker run -d --name "$NAME" --restart unless-stopped \
            -e POSTGRES_PASSWORD="$PASSWORD" \
            -p "127.0.0.1:$PORT:5432" \
            --tmpfs "/var/lib/postgresql/data:rw,size=${KORPIS_DEV_PG_MEMORY:-8g}" \
            "$IMAGE" -c max_connections=300 -c fsync=off -c synchronous_commit=off \
            -c full_page_writes=off >/dev/null
    elif [ "$(docker container inspect -f '{{.State.Running}}' "$NAME")" != true ]; then
        docker start "$NAME" >/dev/null
    fi
    for _ in $(seq 1 60); do
        # Over TCP: the image's first start runs a server on its socket alone while it initializes,
        # then restarts it, so the socket answers before the server clients will reach is up.
        if docker exec "$NAME" pg_isready -h 127.0.0.1 -U postgres >/dev/null 2>&1; then
            echo "$URL"; exit 0
        fi
        sleep 0.5
    done
    echo "postgres did not become ready" >&2; exit 1
    ;;
down) docker stop "$NAME" >/dev/null ;;
destroy)
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    # The volume an earlier version of this script kept its data in.
    docker volume rm "$VOLUME" >/dev/null 2>&1 || true
    ;;
url) echo "$URL" ;;
psql) exec psql "$URL" ;;
*) sed -n '2,10p' "$0"; exit 2 ;;
esac
