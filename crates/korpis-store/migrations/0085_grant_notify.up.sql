-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A grant issued to a workload, or revoked or suspended anywhere above one a workload holds,
-- announces that workload (§4.3 of docs/02-architecture.md): whether its node relays the control
-- plane's API to it is read from the grants it holds (§9.5 of docs/08-identity.md), so its
-- assignment follows the change at once rather than at the next resync. Revocation is written
-- on one record and derived for its descendants (§3.5 there), so the descendants are walked
-- here. A grant's own expiry announces nothing; the resync finds it.

CREATE FUNCTION korpis_notify_grant_workloads() RETURNS trigger
    LANGUAGE plpgsql AS $$
DECLARE
    held record;
BEGIN
    FOR held IN
        WITH RECURSIVE tree AS (
            SELECT id, subject_key FROM grants WHERE id = NEW.id
            UNION
            SELECT g.id, g.subject_key FROM grants g JOIN tree t ON g.parent_id = t.id
        )
        SELECT DISTINCT substr(subject_key, length('workload:') + 1) AS workload
          FROM tree WHERE subject_key LIKE 'workload:%'
    LOOP
        PERFORM pg_notify('korpis_workload', held.workload);
    END LOOP;
    RETURN NULL;
END;
$$;

CREATE TRIGGER grant_notify AFTER INSERT OR UPDATE OF revoked_at, suspended_at ON grants
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_grant_workloads();
