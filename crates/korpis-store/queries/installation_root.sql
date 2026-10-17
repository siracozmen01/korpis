-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The installation's root organization: the first created with no parent.
SELECT id FROM organization WHERE parent_id IS NULL ORDER BY created, id LIMIT 1
