-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The quota entries of a project ($1) and of organizations ($2) that name the nodes they count,
-- read under the placement lock, which every change to them holds.
SELECT id, owner_kind, owner_id, selector, nodes, reservations, limits, version, held,
       egress_budget
  FROM quota_entry
 WHERE ((owner_kind = 'project' AND owner_id = $1)
        OR (owner_kind = 'organization' AND owner_id = ANY($2)))
   AND nodes IS NOT NULL
 ORDER BY id
