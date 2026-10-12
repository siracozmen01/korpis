-- SPDX-License-Identifier: AGPL-3.0-or-later
-- A workload's plans, oldest first, after a keyset cursor.
SELECT id, workload_id, kind, operation_id, from_intent, to_intent, to_power, computed_at,
       computed_by, steps, impact, status, completed, apply_after, expires_at, approved_by,
       approved_at, proposed_under, approval, approval_scope, approval_reason
  FROM plan
 WHERE workload_id = $1 AND ($2::uuid IS NULL OR id > $2)
 ORDER BY id LIMIT $3
