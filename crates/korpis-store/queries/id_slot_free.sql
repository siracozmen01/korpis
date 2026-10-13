-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The lowest id slot on a node that no workload holds, below the number of slots.
SELECT s::integer AS slot
  FROM generate_series(0, $2::bigint - 1) AS s
 WHERE NOT EXISTS (SELECT 1 FROM node_id_slot WHERE node_id = $1 AND slot = s)
 ORDER BY s
 LIMIT 1
