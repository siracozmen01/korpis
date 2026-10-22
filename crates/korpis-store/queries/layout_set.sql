-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Sets organization $1's first layout, at version $4; nothing if it already has one.
INSERT INTO organization_layout (organization_id, name, items, version, set_by, set_at)
VALUES ($1, $2, $3, $4, $5, $6)
ON CONFLICT (organization_id) DO NOTHING
