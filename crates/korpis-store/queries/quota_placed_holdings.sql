-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The placed workloads not deleted in a project ($1) or under an organization and its
-- descendants ($2), with their labels, their node's labels, and what their placement was counted
-- as holding.
WITH RECURSIVE sub AS (
    SELECT id FROM organization WHERE id = $2
    UNION ALL
    SELECT o.id FROM organization o JOIN sub ON o.parent_id = sub.id
)
SELECT w.id AS "id!", w.labels AS "labels!", n.labels AS "node_labels!", h.node_id AS "node!",
       h.held AS "held!", h.counted_by AS "counted_by!"
  FROM workload w
  JOIN placement_held h ON h.workload_id = w.id
  JOIN node n ON n.id = h.node_id
 WHERE w.deleted_at IS NULL
   AND ((w.project_id = $1) OR (w.organization_id IN (SELECT id FROM sub)))
 ORDER BY w.id
