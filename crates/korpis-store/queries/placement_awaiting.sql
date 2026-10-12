-- Workloads not deleted whose approved target has no authoritative placement bound for it, in
-- identifier order after $1, at most $2.
SELECT w.id
  FROM workload w
 WHERE w.deleted_at IS NULL AND w.target_id IS NOT NULL
   AND NOT EXISTS (SELECT 1 FROM placement p
                    WHERE p.workload_id = w.id AND p.authoritative AND p.intent_id = w.target_id)
   AND ($1::uuid IS NULL OR w.id > $1)
 ORDER BY w.id LIMIT $2
