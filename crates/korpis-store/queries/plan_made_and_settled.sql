-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Whether workload $1 was made on a node, an intent or remake plan of it having applied, and no
-- plan of it is approved, queued, or applying (§2.4 of docs/05-scheduling.md).
SELECT EXISTS (SELECT 1 FROM plan
                WHERE workload_id = $1 AND kind IN ('intent', 'remake') AND status = 'applied')
   AND NOT EXISTS (SELECT 1 FROM plan
                    WHERE workload_id = $1 AND status IN ('approved', 'queued', 'applying'))
       AS "made!"
