-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Plans that may have a control-plane step due: approved or applying, with a control-plane step
-- not yet completed. Whether that step is the next one, and whether it is due, the caller decides.
SELECT id, workload_id, kind, operation_id, from_intent, from_power, to_intent, to_power,
       computed_at, computed_by, steps, impact, status, completed, apply_after, expires_at,
       approved_by, approved_at, proposed_under, approval, approval_scope, approval_reason, to_volume,
       to_repository
  FROM plan p
 WHERE p.status IN ('approved', 'applying')
   AND EXISTS (
        SELECT 1 FROM jsonb_array_elements(p.steps) AS s(step)
         WHERE s.step ->> 'executor' = 'control_plane'
           AND (s.step ->> 'index')::bigint > COALESCE(p.completed, -1))
 ORDER BY p.id
