-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A suspension that stops the workloads is derived, like the one that suspends the grants
-- (§2.1 of docs/08-identity.md): every assignment beneath a suspended organization whose
-- suspension stops is computed stopped, and resuming lifts it, so nothing per workload is
-- recorded and the table 0087 kept for it goes. An organization suspended or resumed therefore
-- announces every workload beneath it as well as every workload holding a grant scoped in its
-- subtree, so each of their assignments follows the change at once.

DROP TABLE suspension_stop;

CREATE OR REPLACE FUNCTION korpis_notify_organization_workloads() RETURNS trigger
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
        SELECT substr(g.subject_key, length('workload:') + 1) AS workload
          FROM grants g JOIN objects o ON g.scope_kind = o.kind AND g.scope_id = o.id
         WHERE g.subject_key LIKE 'workload:%' AND g.revoked_at IS NULL
        UNION
        SELECT id::text FROM workload
         WHERE project_id IN (SELECT id FROM projects) AND deleted_at IS NULL
    LOOP
        PERFORM pg_notify('korpis_workload', held.workload);
    END LOOP;
    RETURN NULL;
END;
$$;

