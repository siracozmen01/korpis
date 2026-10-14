-- Ends the attempt of job $1 held by worker $2 as $3: done, failed, or pending again at $4. A
-- retry past the job's last attempt fails it instead. $5 is why, $6 is now. Nothing changes for a
-- worker whose lease ran out and whose job another worker took.
UPDATE job
   SET status = CASE
           WHEN $3::text = 'pending' AND attempts >= max_attempts THEN 'failed'
           ELSE $3::text
       END,
       run_after = COALESCE($4::timestamptz, run_after),
       last_error = COALESCE($5::jsonb, last_error),
       locked_by = NULL,
       locked_until = NULL,
       finished_at = CASE
           WHEN $3::text = 'pending' AND attempts < max_attempts THEN NULL
           ELSE $6::timestamptz
       END
 WHERE id = $1 AND status = 'running' AND locked_by = $2
