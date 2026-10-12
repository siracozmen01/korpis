-- The newest attempt that found no node for workload $1's target $2, returning the intent the
-- previous record was for, if any: the statement's snapshot still sees that row.
WITH previous AS (SELECT intent_id FROM placement_failure WHERE workload_id = $1)
INSERT INTO placement_failure (workload_id, intent_id, attempted_at, explanation)
VALUES ($1, $2, $3, $4)
ON CONFLICT (workload_id) DO UPDATE SET
    intent_id = EXCLUDED.intent_id, attempted_at = EXCLUDED.attempted_at,
    explanation = EXCLUDED.explanation
RETURNING (SELECT intent_id FROM previous) AS previous
