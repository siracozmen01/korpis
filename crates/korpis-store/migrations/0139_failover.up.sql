-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Failover (§2.4 of docs/05-scheduling.md): a `stop` workload whose node went silent is placed
-- again, once its lease is over at the grantor's end, on a node that reaches all its data, and
-- its placement says so. A workload made on one node and placed on another, by failover or away
-- from a node removed as compromised, is made there by a remake plan (Finding 204 of
-- docs/23-walkthroughs.md).

ALTER TABLE placement DROP CONSTRAINT placement_bound_by_check;
ALTER TABLE placement
    ADD CONSTRAINT placement_bound_by_check
        CHECK (bound_by IN ('scheduler', 'operator', 'migration', 'recovery', 'failover'));

ALTER TABLE plan DROP CONSTRAINT plan_kind_check;
ALTER TABLE plan ADD CONSTRAINT plan_kind_check
    CHECK (kind IN ('intent', 'power', 'retire', 'migrate', 'remake', 'delete_volume',
                    'snapshot_volume', 'rollback_volume', 'delete_snapshot', 'backup_volume',
                    'restore_volume', 'prune_backups'));
