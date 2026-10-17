-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The control plane reads which migrations are applied before it serves, and refuses a schema it
-- was not built for (§4 of docs/18-operations.md). It connects as a login that is a member of
-- korpis_app and owns nothing (§3.1 of docs/03-state.md), so korpis_app may read the record of
-- migrations, and nothing more of it.

GRANT SELECT ON _sqlx_migrations TO korpis_app;
