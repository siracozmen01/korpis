-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The layout organization $1 shows: its own, or the nearest ancestor's, skipping any organization
-- that removed its own.
WITH RECURSIVE up AS (
    SELECT id, parent_id, 0 AS depth FROM organization WHERE id = $1
    UNION ALL
    SELECT o.id, o.parent_id, up.depth + 1 FROM organization o JOIN up ON o.id = up.parent_id
)
SELECT l.organization_id, l.name, l.items
  FROM up JOIN organization_layout l ON l.organization_id = up.id AND NOT l.inherit
 ORDER BY up.depth
 LIMIT 1
