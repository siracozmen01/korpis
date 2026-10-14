-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The job queue of §6 of docs/03-state.md, made in Phase 0 and first used now, when prunes are
-- the first work the control plane does itself: a job is claimed by one replica at a time with
-- FOR UPDATE SKIP LOCKED under a lease that a replica which died holding it outlives, retried
-- with backoff up to its bound, and finished once. Queueing, and putting a job back for a later
-- attempt, announce on `korpis_job`, which wakes the workers; they also look on a slow timer, so a
-- missed announcement delays a job and never strands it.

ALTER TABLE job ADD COLUMN finished_at timestamptz;
ALTER TABLE job ADD CONSTRAINT job_attempts CHECK (attempts >= 0 AND max_attempts > 0);
ALTER TABLE job ADD CONSTRAINT job_locked CHECK (
    (status = 'running') = (locked_by IS NOT NULL AND locked_until IS NOT NULL)
);
ALTER TABLE job ADD CONSTRAINT job_finished CHECK (
    (status IN ('succeeded', 'failed', 'cancelled')) = (finished_at IS NOT NULL)
);
DROP INDEX job_ready;
CREATE INDEX job_due ON job (run_after) WHERE status IN ('pending', 'running');

CREATE FUNCTION korpis_job_announce() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('korpis_job', '');
    RETURN NULL;
END;
$$;

CREATE TRIGGER job_queued AFTER INSERT ON job
    FOR EACH ROW EXECUTE FUNCTION korpis_job_announce();
CREATE TRIGGER job_requeued AFTER UPDATE OF run_after ON job
    FOR EACH ROW WHEN (NEW.status = 'pending') EXECUTE FUNCTION korpis_job_announce();
