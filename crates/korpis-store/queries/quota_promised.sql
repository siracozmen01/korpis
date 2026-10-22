-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The quota entries of the organizations directly beneath the organizations $1, with the parent
-- each is beneath: what those parents promised them (§5.1 of docs/05-scheduling.md, Finding 139).
-- Read without a lock: every change to what one of them counts, or to what it was given, takes
-- the lock of its parent's entry of its shape first, which the caller holds.
SELECT e.id, e.owner_kind, e.owner_id, e.selector, e.nodes, e.reservations, e.limits, e.version,
       e.held, e.egress_budget, o.parent_id AS "parent!"
  FROM quota_entry e
  JOIN organization o ON e.owner_kind = 'organization' AND o.id = e.owner_id
 WHERE o.parent_id = ANY($1)
 ORDER BY e.id
