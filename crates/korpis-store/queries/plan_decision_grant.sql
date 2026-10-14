-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The grant of the plan whose decision a workload's placement and lease carry out: the plan that
-- approved its current target or, with no target, the plan most recently approved (a retirement,
-- or a first plan cancelled before it applied), never one deleting a retained volume. Read in the
-- deciding transaction, which sees the approval it is making.
SELECT p.proposed_under
  FROM plan p
  JOIN workload w ON w.id = p.workload_id
 WHERE p.workload_id = $1
   AND p.approved_at IS NOT NULL
   AND p.proposed_under IS NOT NULL
   AND p.kind <> 'delete_volume'
   AND (w.target_id IS NULL OR p.to_intent = w.target_id)
 ORDER BY p.approved_at DESC, p.id DESC
 LIMIT 1
