-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The devices held on each of nodes $1 by placements of workloads other than $2: what the
-- scheduler gives none of again (§6.3 of docs/05-scheduling.md). Both of a migration's placements
-- hold theirs.
SELECT node_id, devices FROM placement
 WHERE node_id = ANY($1) AND workload_id <> $2 AND devices <> '[]'::jsonb
