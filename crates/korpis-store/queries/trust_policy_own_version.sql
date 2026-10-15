-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The version of organization $1's own policy row, set or removed.
SELECT version FROM trust_policy WHERE organization_id = $1
