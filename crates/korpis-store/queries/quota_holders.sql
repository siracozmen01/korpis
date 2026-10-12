-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The workloads not deleted in a project ($1) or under an organization and its descendants ($2),
-- except one ($3), with the bodies of their head and target intents.
WITH RECURSIVE sub AS (
    SELECT id FROM organization WHERE id = $2
    UNION ALL
    SELECT o.id FROM organization o JOIN sub ON o.parent_id = sub.id
)
SELECT w.id AS "id!", w.labels AS "labels!",
       h.body::text AS "head!", h.schema_version AS "head_schema!",
       t.body::text AS "target?", t.schema_version AS "target_schema?"
  FROM workload w
  JOIN intent h ON h.id = w.intent_id
  LEFT JOIN intent t ON t.id = w.target_id
 WHERE w.deleted_at IS NULL
   AND w.id <> $3
   AND ((w.project_id = $1) OR (w.organization_id IN (SELECT id FROM sub)))
