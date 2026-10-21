-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The quota entries of organizations ($1) and projects ($2), locked in identifier order.
SELECT id, owner_kind, owner_id, selector, nodes, reservations, limits, version, held,
       egress_budget
  FROM quota_entry
 WHERE (owner_kind = 'organization' AND owner_id = ANY($1))
    OR (owner_kind = 'project' AND owner_id = ANY($2))
 ORDER BY id
   FOR UPDATE
