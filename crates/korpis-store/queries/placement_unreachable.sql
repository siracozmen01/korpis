-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Workloads not deleted whose authoritative placement, not held by a restore, is on a node the
-- silence sweep marked unreachable (§2.4 of docs/05-scheduling.md), in identifier order after
-- $1, at most $2.
SELECT p.workload_id
  FROM placement p
  JOIN node n ON n.id = p.node_id
  JOIN workload w ON w.id = p.workload_id
 WHERE n.connectivity = 'unreachable' AND p.authoritative AND p.held_at IS NULL
   AND w.deleted_at IS NULL
   AND ($1::uuid IS NULL OR p.workload_id > $1)
 ORDER BY p.workload_id LIMIT $2
