-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The version of organization $1's own theme row, set or removed.
SELECT version FROM organization_theme WHERE organization_id = $1
