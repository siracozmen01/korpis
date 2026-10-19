-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The ancestry of one object ($1 kind, $2 id): the object first, then each ancestor up to its
-- root organization, with labels. A node's parent is the organization it was enrolled into, and a
-- volume's the project it belongs to, a repository's its project, or its organization when it is
-- scoped to the whole of it, and a domain's or a certificate's the organization holding it. One
-- statement walks from the object to its organization and then up the organization tree, so the
-- answer is consistent with a single snapshot. An organization says whether it is suspended,
-- which authorization derives every grant's suspension from (§2.1 of docs/08-identity.md).
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
    SELECT 'node', n.id, n.labels, 0, NULL, n.organization_id
      FROM node n WHERE $1 = 'node' AND n.id = $2
    UNION ALL
    SELECT 'volume', v.id, '{}'::jsonb, 0, v.project_id, NULL
      FROM volume v WHERE $1 = 'volume' AND v.id = $2
    UNION ALL
    SELECT 'repository', r.id, '{}'::jsonb, 0, r.project_id,
           CASE WHEN r.project_id IS NULL THEN r.organization_id END
      FROM repository r WHERE $1 = 'repository' AND r.id = $2
    UNION ALL
    SELECT 'domain', d.id, '{}'::jsonb, 0, NULL, d.organization_id
      FROM domain d WHERE $1 = 'domain' AND d.id = $2
    UNION ALL
    SELECT 'certificate', c.id, '{}'::jsonb, 0, NULL, c.organization_id
      FROM certificate c WHERE $1 = 'certificate' AND c.id = $2
),
project_step AS (
    SELECT 'project'::text AS kind, p.id, p.labels, 1 AS depth,
           p.organization_id AS up_org
      FROM start s JOIN project p ON p.id = s.up_project
),
first_org AS (
    SELECT up_org AS id, depth FROM project_step
    UNION ALL
    SELECT up_org, depth FROM start
     WHERE kind IN ('project', 'organization', 'node', 'domain', 'certificate')
        OR (kind = 'repository' AND up_org IS NOT NULL)
),
orgs AS (
    SELECT o.id, o.parent_id, f.depth + 1 AS depth
      FROM first_org f JOIN organization o ON o.id = f.id
    UNION ALL
    SELECT o.id, o.parent_id, orgs.depth + 1
      FROM orgs JOIN organization o ON o.id = orgs.parent_id
)
SELECT chain.kind AS "kind!", chain.id AS "id!", chain.labels AS "labels!",
       chain.depth AS "depth!",
       (chain.kind = 'organization' AND coalesce(o.status = 'suspended', false))
           AS "suspended!"
  FROM (
    SELECT kind, id, labels, depth FROM start
    UNION ALL
    SELECT kind, id, labels, depth FROM project_step
    UNION ALL
    SELECT 'organization', id, '{}'::jsonb, depth FROM orgs
  ) chain
  LEFT JOIN organization o ON chain.kind = 'organization' AND o.id = chain.id
 ORDER BY chain.depth
