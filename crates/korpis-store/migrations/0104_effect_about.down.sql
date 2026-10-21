-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0104: effects name no object, organization, or project again, and an object's
-- organization is resolved for the kinds 0040 knew.

DROP INDEX effect_project;
DROP INDEX effect_organization;
DROP FUNCTION korpis_object_project(text, uuid);
CREATE OR REPLACE FUNCTION korpis_object_organization(kind text, object uuid) RETURNS uuid
LANGUAGE sql STABLE AS $$
    SELECT CASE kind
        WHEN 'organization' THEN object
        WHEN 'project' THEN (SELECT organization_id FROM project WHERE id = object)
        WHEN 'workload' THEN (SELECT organization_id FROM workload WHERE id = object)
        WHEN 'node' THEN (SELECT organization_id FROM node WHERE id = object)
        WHEN 'volume' THEN (SELECT p.organization_id FROM volume v
                              JOIN project p ON p.id = v.project_id WHERE v.id = object)
        WHEN 'repository' THEN (SELECT organization_id FROM repository WHERE id = object)
    END
$$;
ALTER TABLE effect
    DROP CONSTRAINT effect_object,
    DROP COLUMN project_id,
    DROP COLUMN organization_id,
    DROP COLUMN object_id,
    DROP COLUMN object_kind;
