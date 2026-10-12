-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Every change to what a workload's status is read from announces the workload, on the channel
-- `korpis_workload`, when its transaction commits (§4.3 of docs/02-architecture.md). A watcher
-- re-reads the workloads it watches when they are announced instead of polling them.
--
-- The announcement is made by the database, not by the code that writes, so a writer added later
-- (the agent protocol's ingestion, an operator's tool) cannot forget it. NOTIFY is transactional:
-- nothing is announced for a transaction that rolls back, and repeats of one payload within a
-- transaction are delivered once. It is a wake-up, not a record: a listener that was disconnected
-- re-reads everything it watches, because what it missed is not replayed.

CREATE FUNCTION korpis_notify_workload() RETURNS trigger
    LANGUAGE plpgsql AS $$
DECLARE
    row_data record;
    workload uuid;
BEGIN
    IF TG_OP = 'DELETE' THEN
        row_data := OLD;
    ELSE
        row_data := NEW;
    END IF;
    IF TG_TABLE_NAME = 'workload' THEN
        workload := row_data.id;
    ELSE
        workload := row_data.workload_id;
    END IF;
    PERFORM pg_notify('korpis_workload', workload::text);
    RETURN NULL;
END;
$$;

CREATE TRIGGER workload_notify AFTER INSERT OR UPDATE ON workload
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_workload();
CREATE TRIGGER plan_notify AFTER INSERT OR UPDATE ON plan
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_workload();
CREATE TRIGGER power_notify AFTER INSERT ON power
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_workload();
CREATE TRIGGER placement_notify AFTER INSERT OR UPDATE OR DELETE ON placement
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_workload();
CREATE TRIGGER placement_failure_notify AFTER INSERT OR UPDATE OR DELETE ON placement_failure
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_workload();
CREATE TRIGGER observation_notify AFTER INSERT OR UPDATE ON observation_latest
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_workload();

-- A failed placement attempt keeps the moment its explanation was first reached, not the moment it
-- was last retried: a retry that reaches the same explanation writes nothing, so it announces
-- nothing, and a watcher hears when the reason changes rather than on every retry pass.
ALTER TABLE placement_failure RENAME COLUMN attempted_at TO since;
