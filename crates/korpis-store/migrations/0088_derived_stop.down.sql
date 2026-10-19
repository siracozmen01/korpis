-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0088.

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
        SELECT DISTINCT substr(g.subject_key, length('workload:') + 1) AS workload
          FROM grants g JOIN objects o ON g.scope_kind = o.kind AND g.scope_id = o.id
         WHERE g.subject_key LIKE 'workload:%' AND g.revoked_at IS NULL
    LOOP
        PERFORM pg_notify('korpis_workload', held.workload);
    END LOOP;
    RETURN NULL;
END;
$$;

CREATE TABLE suspension_stop (
    organization_id  uuid NOT NULL REFERENCES organization (id),
    workload_id      uuid NOT NULL REFERENCES workload (id),
    power_version    bigint NOT NULL CHECK (power_version > 0),
    PRIMARY KEY (organization_id, workload_id)
);

GRANT SELECT, INSERT, DELETE ON suspension_stop TO korpis_app;
