-- SPDX-License-Identifier: AGPL-3.0-or-later
-- What the placements on each node of $1 reserve, summed per dimension. Authoritative or not:
-- during a migration both nodes hold the workload's capacity. A replica a node keeps of a live
-- volume reserves the volume's size on its disk (§2.2 of docs/06-storage.md).
SELECT a.node_id AS "node_id!", a.dimension AS "dimension!", sum(a.value)::bigint AS "total!"
  FROM (SELECT p.node_id, r.key AS dimension, r.value::bigint AS value
          FROM placement p, jsonb_each_text(p.reserved) r
         WHERE p.node_id = ANY($1)
        UNION ALL
        SELECT k.node_id, 'disk_bytes', v.size_bytes
          FROM volume_replica k JOIN volume v ON v.id = k.volume_id
         WHERE k.node_id = ANY($1) AND v.deleted_at IS NULL) a
 GROUP BY a.node_id, a.dimension
