-- SPDX-License-Identifier: AGPL-3.0-or-later
-- A placement: new, or the one on the same node following its workload's target in place, with
-- the devices it holds ($10).
INSERT INTO placement (workload_id, node_id, intent_id, reserved, bound_at, bound_by, reason,
                       explanation, authoritative, devices)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
ON CONFLICT (workload_id, node_id) DO UPDATE SET
    intent_id = EXCLUDED.intent_id, reserved = EXCLUDED.reserved, bound_at = EXCLUDED.bound_at,
    bound_by = EXCLUDED.bound_by, reason = EXCLUDED.reason, explanation = EXCLUDED.explanation,
    authoritative = EXCLUDED.authoritative, devices = EXCLUDED.devices
