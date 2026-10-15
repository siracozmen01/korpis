-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The workloads of organization $1 other than $2 with an authoritative placement, and where.
SELECT w.id AS workload, w.project_id AS project, w.labels, p.node_id AS node
  FROM placement p JOIN workload w ON w.id = p.workload_id
 WHERE p.authoritative AND w.organization_id = $1 AND w.id <> $2 AND w.deleted_at IS NULL
