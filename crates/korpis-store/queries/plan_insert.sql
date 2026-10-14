-- SPDX-License-Identifier: AGPL-3.0-or-later
-- A new plan. Its decision columns (approval, schedule, progress) are written by the update that
-- follows, so that there is one statement that writes them.
INSERT INTO plan (id, workload_id, kind, operation_id, from_intent, from_power, to_intent, to_power,
                  computed_at, computed_by, steps, impact, status, expires_at, proposed_under,
                  approval, approval_scope, approval_reason, to_volume, to_repository)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19,
        $20)
