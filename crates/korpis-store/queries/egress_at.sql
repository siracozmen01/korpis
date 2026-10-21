-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Every workload placed now ($1 and $2 bounding the month) or with egress recorded in the month,
-- on each node, with what its records hold for it there as an invoice sums them: a measurement
-- in the month its interval begins, an adjustment in the month it was recorded, and a record
-- superseded only by an adjustment still counted (§2 of docs/15-observability.md).
WITH spent AS (
    SELECT r.workload_id, r.node_id, sum(r.egress_bytes)::bigint AS bytes
      FROM metering_record r
     WHERE r.egress_bytes IS NOT NULL
       AND (r.kind <> 'adjustment' AND r.interval_start >= $1 AND r.interval_start < $2
            OR r.kind = 'adjustment' AND r.recorded_at >= $1 AND r.recorded_at < $2)
       AND NOT EXISTS (SELECT 1 FROM metering_record s
                        WHERE r.id = ANY (s.supersedes) AND s.kind <> 'adjustment')
     GROUP BY r.workload_id, r.node_id
), placed AS (
    SELECT workload_id, node_id FROM placement WHERE authoritative
), at AS (
    SELECT workload_id, node_id FROM spent
    UNION
    SELECT workload_id, node_id FROM placed
)
SELECT w.id AS workload_id, w.project_id, w.organization_id, w.labels,
       a.node_id AS "node_id?", n.labels AS "node_labels?",
       coalesce(s.bytes, 0) AS "bytes!",
       (p.workload_id IS NOT NULL) AS "placed!"
  FROM at a
  JOIN workload w ON w.id = a.workload_id
  LEFT JOIN node n ON n.id = a.node_id
  LEFT JOIN spent s ON s.workload_id = a.workload_id AND s.node_id IS NOT DISTINCT FROM a.node_id
  LEFT JOIN placed p ON p.workload_id = a.workload_id AND p.node_id = a.node_id
 ORDER BY w.id, a.node_id
