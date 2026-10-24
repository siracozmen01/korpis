-- SPDX-License-Identifier: AGPL-3.0-or-later
-- One backup ($1), or a workload's ($2) backups, newest first after a cursor ($4), at most $5;
-- pruned ones only when $3. A backup still pending whose plan ended without it is failed: the
-- plan was cancelled, refused, or failed before its node reported, and nothing will.
SELECT b.id, b.repository_id, b.workload_id, b.volume_id, b.plan_id, b.parent_id,
       CASE WHEN b.status = 'pending'
                 AND p.status IN ('applied', 'failed', 'cancelled', 'rejected', 'expired',
                                  'superseded')
            THEN 'failed' ELSE b.status END AS "status!",
       b.consistent, b.files, b.directories, b.bytes, b.chunks_new, b.bytes_new,
       b.stored_bytes, b.files_unchanged, b.skipped,
       COALESCE(b.error,
                CASE WHEN b.status = 'pending'
                          AND p.status IN ('applied', 'failed', 'cancelled', 'rejected',
                                           'expired', 'superseded')
                     THEN 'its plan ended without it' END) AS error,
       b.created_at, b.finished_at, b.deleted_at, b.image_filesystem
  FROM backup b JOIN plan p ON p.id = b.plan_id
 WHERE ($1::uuid IS NULL OR b.id = $1)
   AND ($2::uuid IS NULL OR b.workload_id = $2)
   AND ($3 OR b.deleted_at IS NULL)
   AND ($4::uuid IS NULL OR b.id < $4)
 ORDER BY b.id DESC
 LIMIT $5
