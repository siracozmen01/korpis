-- One backup ($1), or a workload's ($2) backups, newest first after a cursor ($4), at most $5;
-- pruned ones only when $3.
SELECT id, repository_id, workload_id, volume_id, plan_id, parent_id, status, consistent,
       files, directories, bytes, chunks_new, bytes_new, stored_bytes, files_unchanged,
       skipped, error, created_at, finished_at, deleted_at
  FROM backup
 WHERE ($1::uuid IS NULL OR id = $1)
   AND ($2::uuid IS NULL OR workload_id = $2)
   AND ($3 OR deleted_at IS NULL)
   AND ($4::uuid IS NULL OR id < $4)
 ORDER BY id DESC
 LIMIT $5
