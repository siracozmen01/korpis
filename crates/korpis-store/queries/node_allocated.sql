-- SPDX-License-Identifier: AGPL-3.0-or-later
-- What the placements on node $1 reserve, summed per dimension. Authoritative or not: during a
-- migration both nodes hold the workload's capacity. A replica the node keeps of a live volume
-- reserves the volume's size on its disk (§2.2 of docs/06-storage.md).
SELECT a.dimension AS "dimension!", sum(a.value)::bigint AS "total!"
  FROM (SELECT r.key AS dimension, r.value::bigint AS value
          FROM placement p, jsonb_each_text(p.reserved) r
         WHERE p.node_id = $1
        UNION ALL
        SELECT 'disk_bytes', v.size_bytes
          FROM volume_replica k JOIN volume v ON v.id = k.volume_id
         WHERE k.node_id = $1 AND v.deleted_at IS NULL) a
 GROUP BY a.dimension
