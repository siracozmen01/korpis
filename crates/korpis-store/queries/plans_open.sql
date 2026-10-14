-- SPDX-License-Identifier: AGPL-3.0-or-later
-- A workload's plans still in play: proposed, approved, queued, or applying.
SELECT id, workload_id, kind, operation_id, from_intent, from_power, to_intent, to_power,
       computed_at, computed_by, steps, impact, status, completed, apply_after, expires_at,
       approved_by, approved_at, proposed_under, approval, approval_scope, approval_reason, to_volume,
       to_repository
  FROM plan
 WHERE workload_id = $1 AND status IN ('proposed', 'approved', 'queued', 'applying')
 ORDER BY id
