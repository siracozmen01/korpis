-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The workloads not deleted in a project ($1) or under an organization and its descendants ($2)
-- that have been counted, with what they were counted as holding, locked.
WITH RECURSIVE sub AS (
    SELECT id FROM organization WHERE id = $2
    UNION ALL
    SELECT o.id FROM organization o JOIN sub ON o.parent_id = sub.id
)
SELECT w.id AS "id!", w.labels AS "labels!", h.held AS "held!", h.counted_by AS "counted_by!"
  FROM workload w
  JOIN workload_held h ON h.workload_id = w.id
 WHERE w.deleted_at IS NULL
   AND ((w.project_id = $1) OR (w.organization_id IN (SELECT id FROM sub)))
 ORDER BY w.id
   FOR UPDATE OF h
