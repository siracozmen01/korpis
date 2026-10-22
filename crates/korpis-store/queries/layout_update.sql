-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Replaces organization $1's layout, or its removal, at version $4, if it is still at version $7.
-- A removal is $8: the layout is then cleared and the ancestors' layout applies.
UPDATE organization_layout SET name = $2, items = $3, version = $4, set_by = $5, set_at = $6,
                               inherit = $8
 WHERE organization_id = $1 AND version = $7
