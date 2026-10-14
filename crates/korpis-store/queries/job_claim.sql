-- Claims the oldest due job of kinds $1 at $2 for worker $3 until $4: one pending and past its
-- time, or one running under a lease that ran out. Its attempt is counted as it is claimed.
UPDATE job
   SET status = 'running', locked_by = $3, locked_until = $4, attempts = attempts + 1
 WHERE id = (
        SELECT id FROM job
         WHERE kind = ANY($1)
           AND ((status = 'pending' AND run_after <= $2)
                OR (status = 'running' AND locked_until <= $2))
         ORDER BY run_after
         LIMIT 1
           FOR UPDATE SKIP LOCKED)
RETURNING id, kind, payload, attempts, max_attempts
