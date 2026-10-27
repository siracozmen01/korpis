-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The replicas of each volume of $1, by volume and node (§2.2 of docs/06-storage.md).
SELECT volume_id, node_id, chosen_at, snapshot, received_at, sending, sending_since, stale_at
  FROM volume_replica
 WHERE volume_id = ANY($1)
 ORDER BY volume_id, node_id
