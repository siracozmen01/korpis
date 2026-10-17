-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Sets organization $1's first theme, at version $9; nothing if it already has one.
INSERT INTO organization_theme (organization_id, product_name, logo_media_type, logo, light, dark,
                                sign_in_text, stylesheet, version, set_by, set_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
ON CONFLICT (organization_id) DO NOTHING
