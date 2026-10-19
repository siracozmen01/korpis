-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- An organization suspended or resumed announces every workload that holds a grant scoped in its
-- subtree (§4.3 of docs/02-architecture.md): such a grant is suspended by derivation from the
-- organization's status (§2.1 of docs/08-identity.md), and whether a workload's node relays the
-- control plane's API to it is read from the grants it holds (§9.5 there), so its assignment
-- follows the change at once rather than at the next resync. The subtree is every organization
-- beneath it and every object a grant can be scoped at that belongs to one of them.

CREATE FUNCTION korpis_notify_organization_workloads() RETURNS trigger
    LANGUAGE plpgsql AS $$
DECLARE
    held record;
BEGIN
    FOR held IN
        WITH RECURSIVE orgs AS (
            SELECT id FROM organization WHERE id = NEW.id
            UNION
            SELECT o.id FROM organization o JOIN orgs ON o.parent_id = orgs.id
        ),
        projects AS (
            SELECT id FROM project WHERE organization_id IN (SELECT id FROM orgs)
        ),
        objects AS (
            SELECT 'organization'::text AS kind, id FROM orgs
            UNION ALL SELECT 'project', id FROM projects
            UNION ALL SELECT 'workload', id FROM workload
                       WHERE project_id IN (SELECT id FROM projects)
            UNION ALL SELECT 'node', id FROM node WHERE organization_id IN (SELECT id FROM orgs)
            UNION ALL SELECT 'volume', id FROM volume WHERE project_id IN (SELECT id FROM projects)
            UNION ALL SELECT 'repository', id FROM repository
                       WHERE organization_id IN (SELECT id FROM orgs)
            UNION ALL SELECT 'domain', id FROM domain WHERE organization_id IN (SELECT id FROM orgs)
            UNION ALL SELECT 'certificate', id FROM certificate
                       WHERE organization_id IN (SELECT id FROM orgs)
        )
        SELECT DISTINCT substr(g.subject_key, length('workload:') + 1) AS workload
          FROM grants g JOIN objects o ON g.scope_kind = o.kind AND g.scope_id = o.id
         WHERE g.subject_key LIKE 'workload:%' AND g.revoked_at IS NULL
    LOOP
        PERFORM pg_notify('korpis_workload', held.workload);
    END LOOP;
    RETURN NULL;
END;
$$;

CREATE TRIGGER organization_notify AFTER UPDATE OF status ON organization
    FOR EACH ROW WHEN (OLD.status IS DISTINCT FROM NEW.status)
    EXECUTE FUNCTION korpis_notify_organization_workloads();
