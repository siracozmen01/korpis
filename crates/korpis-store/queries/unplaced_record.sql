-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The newest attempt that found no node for workload $1's target $2, returning the intent the
-- previous record was for, if any: the statement's snapshot still sees that row. A retry that
-- reaches the same intent and explanation leaves the record as it was, `since` included, so a
-- watcher hears when the reason changes and not every time it is retried.
WITH previous AS (SELECT intent_id FROM placement_failure WHERE workload_id = $1)
INSERT INTO placement_failure (workload_id, intent_id, since, explanation)
VALUES ($1, $2, $3, $4)
ON CONFLICT (workload_id) DO UPDATE SET
    intent_id = EXCLUDED.intent_id, since = EXCLUDED.since, explanation = EXCLUDED.explanation
    WHERE placement_failure.intent_id IS DISTINCT FROM EXCLUDED.intent_id
       OR placement_failure.explanation IS DISTINCT FROM EXCLUDED.explanation
RETURNING (SELECT intent_id FROM previous) AS previous
