-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The quota entries of a project ($1) and of organizations ($2), locked in identifier order.
SELECT id, owner_kind, owner_id, selector, reservations, limits, version, held
  FROM quota_entry
 WHERE (owner_kind = 'project' AND owner_id = $1)
    OR (owner_kind = 'organization' AND owner_id = ANY($2))
 ORDER BY id
   FOR UPDATE
