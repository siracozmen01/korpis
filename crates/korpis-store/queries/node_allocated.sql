-- SPDX-License-Identifier: AGPL-3.0-or-later
-- What the placements on node $1 reserve, summed per dimension. Authoritative or not: during a
-- migration both nodes hold the workload's capacity.
SELECT r.key AS "dimension!", sum(r.value::bigint)::bigint AS "total!"
  FROM placement p, jsonb_each_text(p.reserved) r
 WHERE p.node_id = $1
 GROUP BY r.key
