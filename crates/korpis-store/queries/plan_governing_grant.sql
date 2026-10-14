-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The grant of the plan most recently approved for a workload that is applying or applied. A plan
-- deleting a retained volume governs nothing the workload keeps, so it is never the answer.
SELECT proposed_under
  FROM plan
 WHERE workload_id = $1 AND status IN ('applying', 'applied') AND proposed_under IS NOT NULL
   AND kind <> 'delete_volume'
 ORDER BY approved_at DESC NULLS LAST, id DESC
 LIMIT 1
