-- SPDX-License-Identifier: AGPL-3.0-or-later

UPDATE placement SET bound_by = 'scheduler' WHERE bound_by = 'failover';
ALTER TABLE placement DROP CONSTRAINT placement_bound_by_check;
ALTER TABLE placement
    ADD CONSTRAINT placement_bound_by_check
        CHECK (bound_by IN ('scheduler', 'operator', 'migration', 'recovery'));

UPDATE plan SET kind = 'migrate' WHERE kind = 'remake';
ALTER TABLE plan DROP CONSTRAINT plan_kind_check;
ALTER TABLE plan ADD CONSTRAINT plan_kind_check
    CHECK (kind IN ('intent', 'power', 'retire', 'migrate', 'delete_volume', 'snapshot_volume',
                    'rollback_volume', 'delete_snapshot', 'backup_volume', 'restore_volume',
                    'prune_backups'));
