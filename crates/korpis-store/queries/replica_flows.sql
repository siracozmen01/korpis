-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Every replica of a live volume on one node's disk, with the volume's node and the helper's
-- identifier for it (§2.2 of docs/06-storage.md).
SELECT r.volume_id, r.node_id, r.chosen_at, r.snapshot, r.received_at, r.sending,
       r.sending_since, r.stale_at, v.node_id AS from_node, v.helper_id
  FROM volume_replica r JOIN volume v ON v.id = r.volume_id
 WHERE v.deleted_at IS NULL AND v.store IS NULL
 ORDER BY r.volume_id, r.node_id
