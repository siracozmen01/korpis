-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The plan most recently approved to move a workload's power target to a version: whether that
-- power was ever reached (§4.8 of docs/02-architecture.md).
SELECT id, workload_id, kind, operation_id, from_intent, from_power, to_intent, to_power, computed_at,
       computed_by, steps, impact, status, completed, apply_after, expires_at, approved_by,
       approved_at, proposed_under, approval, approval_scope, approval_reason, to_volume,
       to_repository
  FROM plan
 WHERE workload_id = $1 AND to_power = $2 AND approved_at IS NOT NULL
 ORDER BY approved_at DESC, id DESC LIMIT 1
