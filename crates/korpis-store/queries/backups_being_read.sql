-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The backups of workload $2 in repository $1 being read at $3: one an open plan restores (a
-- restore_volume step names it), or one read under access that has not ended.
SELECT (s.step -> 'detail' -> 'backup' ->> 'string')::uuid AS "id!"
  FROM plan p
 CROSS JOIN LATERAL jsonb_array_elements(p.steps) AS s(step)
  JOIN backup b ON b.id = (s.step -> 'detail' -> 'backup' ->> 'string')::uuid
 WHERE p.status IN ('proposed', 'approved', 'queued', 'applying')
   AND s.step ->> 'action' = 'restore_volume'
   AND b.repository_id = $1 AND b.workload_id = $2
UNION
SELECT backup_id AS "id!"
  FROM backup_access
 WHERE repository_id = $1 AND workload_id = $2 AND mode = 'read' AND area = 'backups'
   AND expires_at > $3
