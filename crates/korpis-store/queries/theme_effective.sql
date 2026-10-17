-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The theme organization $1 wears: its own, or the nearest ancestor's, skipping any organization
-- that removed its own.
WITH RECURSIVE up AS (
    SELECT id, parent_id, 0 AS depth FROM organization WHERE id = $1
    UNION ALL
    SELECT o.id, o.parent_id, up.depth + 1 FROM organization o JOIN up ON o.id = up.parent_id
)
SELECT t.organization_id, t.product_name, t.logo_media_type, t.logo, t.light, t.dark,
       t.sign_in_text, t.stylesheet
  FROM up JOIN organization_theme t ON t.organization_id = up.id AND NOT t.inherit
 ORDER BY up.depth
 LIMIT 1
