-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The first device due a call at $1, held until $2 (§2.1 of docs/06-storage.md): one not made
-- yet; one whose workload holds its lease, on the node it is placed on, for another node or
-- epoch than the device was attached for; one attached for a lease no longer held, released or
-- past its grantor's end; and one gone, its volume deleted, or its workload deleted with no
-- volume made of it. With the lease held, its node's way of attaching the device's class, and
-- whether it is gone.
WITH due AS (
    SELECT b.id, h.node_id AS lease_node, h.epoch AS lease_epoch, n.storage_classes AS classes,
           (EXISTS (SELECT 1 FROM volume v WHERE v.id = b.id AND v.deleted_at IS NOT NULL)
            OR (NOT EXISTS (SELECT 1 FROM volume v WHERE v.id = b.id)
                AND EXISTS (SELECT 1 FROM workload w
                             WHERE w.id = b.workload_id AND w.deleted_at IS NOT NULL)))
               AS gone
      FROM block_device b
      LEFT JOIN placement pl ON pl.workload_id = b.workload_id AND pl.authoritative
      LEFT JOIN lease h
        ON h.workload_id = b.workload_id AND h.node_id = pl.node_id AND h.released_at IS NULL
       AND h.grantor_expiry > $1
      LEFT JOIN node n ON n.id = h.node_id
     WHERE b.deleted_at IS NULL
       AND (b.due IS NULL OR b.due <= $1)
       AND (b.held_until IS NULL OR b.held_until <= $1)),
picked AS (
    SELECT due.* FROM due JOIN block_device b ON b.id = due.id
     WHERE b.device IS NULL OR due.gone
        OR b.attached_node_id IS DISTINCT FROM due.lease_node
        OR b.attached_epoch IS DISTINCT FROM due.lease_epoch
     ORDER BY due.id
     LIMIT 1
       FOR UPDATE OF b SKIP LOCKED)
UPDATE block_device b SET held_until = $2
  FROM picked WHERE b.id = picked.id
RETURNING b.id, b.organization_id, b.storage_class, b.size_bytes, b.device, b.attached_node_id,
          b.attached_epoch, b.attempts, picked.lease_node AS "lease_node?",
          picked.lease_epoch AS "lease_epoch?", picked.gone AS "gone!",
          (SELECT c ->> 'block' FROM jsonb_array_elements(picked.classes) c
            WHERE c ->> 'name' = b.storage_class) AS "protocol?"
