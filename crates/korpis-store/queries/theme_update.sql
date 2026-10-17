-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Replaces organization $1's theme, or its removal, at version $9, if it is still at version $12.
-- A removal is $13: the theme columns are then cleared and the ancestors' theme applies.
UPDATE organization_theme SET product_name = $2, logo_media_type = $3, logo = $4, light = $5,
                              dark = $6, sign_in_text = $7, stylesheet = $8, version = $9,
                              set_by = $10, set_at = $11, inherit = $13
 WHERE organization_id = $1 AND version = $12
