-- Takes the prefix of workload $2 in repository $1 for sweep $3, if no backup into it is open and
-- no other sweep holds it. Run after the row is locked FOR UPDATE in a statement of its own, so
-- this one's snapshot sees every backup proposed by a transaction that held the row's share lock
-- (§5.5 of docs/06-storage.md).
UPDATE workload_key k
   SET sweeping_job = $3
 WHERE k.repository_id = $1 AND k.workload_id = $2
   AND (k.sweeping_job IS NULL OR k.sweeping_job = $3)
   AND NOT EXISTS (
        SELECT 1 FROM backup b JOIN plan p ON p.id = b.plan_id
         WHERE b.repository_id = $1 AND b.workload_id = $2 AND b.status = 'pending'
           AND p.status IN ('proposed', 'approved', 'queued', 'applying'))
