-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The organizations a subject ($1, its key) holds a live grant in at $2 (§10 of docs/10-api.md):
-- for each of its grants whose every link up to the root is unrevoked, unsuspended, and unexpired,
-- the organization its scope is anchored in, found as the first step of ancestry.sql finds it.
-- Every grant is read, however many it holds, so none of its organizations goes uncharged.
WITH RECURSIVE
held AS (
    SELECT id, scope_kind, scope_id FROM grants WHERE subject_key = $1
),
chain AS (
    SELECT h.id AS leaf, h.id AS link FROM held h
    UNION ALL
    SELECT c.leaf, g.parent_id
      FROM chain c JOIN grants g ON g.id = c.link
     WHERE g.parent_id IS NOT NULL
),
live AS (
    SELECT c.leaf
      FROM chain c JOIN grants g ON g.id = c.link
     GROUP BY c.leaf
    HAVING bool_and(g.revoked_at IS NULL AND g.suspended_at IS NULL
                    AND (g.conditions->>'expires_at' IS NULL
                         OR (g.conditions->>'expires_at')::timestamptz > $2))
),
anchored AS (
    SELECT CASE h.scope_kind
             WHEN 'organization' THEN h.scope_id
             WHEN 'project' THEN (SELECT organization_id FROM project WHERE id = h.scope_id)
             WHEN 'workload' THEN (SELECT organization_id FROM workload WHERE id = h.scope_id)
             WHEN 'node' THEN (SELECT organization_id FROM node WHERE id = h.scope_id)
             WHEN 'volume' THEN (SELECT p.organization_id
                                   FROM volume v JOIN project p ON p.id = v.project_id
                                  WHERE v.id = h.scope_id)
             WHEN 'repository' THEN (SELECT organization_id FROM repository WHERE id = h.scope_id)
             WHEN 'domain' THEN (SELECT organization_id FROM domain WHERE id = h.scope_id)
             WHEN 'certificate' THEN (SELECT organization_id FROM certificate WHERE id = h.scope_id)
             WHEN 'portal' THEN (SELECT organization_id FROM portal WHERE id = h.scope_id)
           END AS organization
      FROM held h JOIN live l ON l.leaf = h.id
)
SELECT DISTINCT organization AS "organization!"
  FROM anchored
 WHERE organization IS NOT NULL
 ORDER BY 1
