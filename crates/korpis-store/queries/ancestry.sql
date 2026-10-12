-- The ancestry of one object ($1 kind, $2 id): the object first, then each ancestor up to its
-- root organization, with labels. One statement walks from the object to its organization and
-- then up the organization tree, so the answer is consistent with a single snapshot.
WITH RECURSIVE
start AS (
    SELECT 'workload'::text AS kind, w.id, w.labels, 0 AS depth,
           w.project_id AS up_project, NULL::uuid AS up_org
      FROM workload w WHERE $1 = 'workload' AND w.id = $2
    UNION ALL
    SELECT 'project', p.id, p.labels, 0, NULL, p.organization_id
      FROM project p WHERE $1 = 'project' AND p.id = $2
    UNION ALL
    SELECT 'organization', o.id, '{}'::jsonb, 0, NULL, o.parent_id
      FROM organization o WHERE $1 = 'organization' AND o.id = $2
    UNION ALL
    SELECT 'node', n.id, n.labels, 0, NULL, NULL
      FROM node n WHERE $1 = 'node' AND n.id = $2
),
project_step AS (
    SELECT 'project'::text AS kind, p.id, p.labels, 1 AS depth,
           p.organization_id AS up_org
      FROM start s JOIN project p ON p.id = s.up_project
),
first_org AS (
    SELECT up_org AS id, depth FROM project_step
    UNION ALL
    SELECT up_org, depth FROM start WHERE kind IN ('project', 'organization')
),
orgs AS (
    SELECT o.id, o.parent_id, f.depth + 1 AS depth
      FROM first_org f JOIN organization o ON o.id = f.id
    UNION ALL
    SELECT o.id, o.parent_id, orgs.depth + 1
      FROM orgs JOIN organization o ON o.id = orgs.parent_id
)
SELECT kind AS "kind!", id AS "id!", labels AS "labels!", depth AS "depth!"
  FROM (
    SELECT kind, id, labels, depth FROM start
    UNION ALL
    SELECT kind, id, labels, depth FROM project_step
    UNION ALL
    SELECT 'organization', id, '{}'::jsonb, depth FROM orgs
  ) chain
 ORDER BY depth
