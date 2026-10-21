-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- What an effect is about (§4 of docs/15-observability.md): the object its action was authorized
-- over, and the organization and project whose subtree holds it, resolved from the object's own
-- ancestry in the transaction that writes the effect, so that an organization reads its own
-- subtree's effects. An effect about nothing has neither. Effects written before this are about
-- their workload, or else their node, and an earlier one done to an organization itself is about
-- nothing, so it is read only at the root.

ALTER TABLE effect
    ADD COLUMN object_kind     text,
    ADD COLUMN object_id       uuid,
    ADD COLUMN organization_id uuid,
    ADD COLUMN project_id      uuid,
    ADD CONSTRAINT effect_object CHECK ((object_kind IS NULL) = (object_id IS NULL));

-- The organization that owns an object, now for every kind an effect can be about.
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
        WHEN 'endpoint' THEN (SELECT w.organization_id FROM endpoint e
                                JOIN workload w ON w.id = e.workload_id WHERE e.id = object)
        WHEN 'stream' THEN (SELECT w.organization_id FROM stream s
                              JOIN workload w ON w.id = s.workload_id WHERE s.id = object)
        WHEN 'domain' THEN (SELECT organization_id FROM domain WHERE id = object)
        WHEN 'certificate' THEN (SELECT organization_id FROM certificate WHERE id = object)
    END
$$;

-- The project that holds an object, for the kinds a project holds.
CREATE FUNCTION korpis_object_project(kind text, object uuid) RETURNS uuid
LANGUAGE sql STABLE AS $$
    SELECT CASE kind
        WHEN 'project' THEN object
        WHEN 'workload' THEN (SELECT project_id FROM workload WHERE id = object)
        WHEN 'volume' THEN (SELECT project_id FROM volume WHERE id = object)
        WHEN 'repository' THEN (SELECT project_id FROM repository WHERE id = object)
        WHEN 'endpoint' THEN (SELECT w.project_id FROM endpoint e
                                JOIN workload w ON w.id = e.workload_id WHERE e.id = object)
        WHEN 'stream' THEN (SELECT w.project_id FROM stream s
                              JOIN workload w ON w.id = s.workload_id WHERE s.id = object)
    END
$$;

UPDATE effect SET object_kind = 'workload', object_id = workload_id WHERE workload_id IS NOT NULL;
UPDATE effect SET object_kind = 'node', object_id = node_id
 WHERE object_id IS NULL AND node_id IS NOT NULL;
UPDATE effect SET organization_id = korpis_object_organization(object_kind, object_id),
                  project_id = korpis_object_project(object_kind, object_id)
 WHERE object_id IS NOT NULL;

CREATE INDEX effect_organization ON effect (organization_id, at, id);
CREATE INDEX effect_project ON effect (project_id, at, id);
