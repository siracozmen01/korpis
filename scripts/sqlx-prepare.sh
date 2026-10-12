#!/usr/bin/env bash
# Rebuilds the development database from the migrations and regenerates the query metadata in
# .sqlx/, which builds use offline. Run after changing a migration or a query. The database
# named in .env (korpis_dev) is dropped and recreated; nothing else is touched.
set -euo pipefail
cd "$(dirname "$0")/.."

[ -x .tools/bin/sqlx ] || { echo "sqlx-cli missing: run scripts/tools.sh"; exit 1; }
admin=$(./scripts/dev-pg.sh up)
dev_db="${admin%/*}/korpis_dev"
psql "$admin" -qc "DROP DATABASE IF EXISTS korpis_dev WITH (FORCE)" >/dev/null
psql "$admin" -qc "CREATE DATABASE korpis_dev TEMPLATE template0" >/dev/null
DATABASE_URL="$dev_db" .tools/bin/sqlx migrate run --source crates/korpis-store/migrations >/dev/null
DATABASE_URL="$dev_db" PATH="$PWD/.tools/bin:$PATH" cargo sqlx prepare --workspace -- --all-targets --all-features
echo "korpis_dev rebuilt and .sqlx/ regenerated; commit .sqlx/ with the change"
