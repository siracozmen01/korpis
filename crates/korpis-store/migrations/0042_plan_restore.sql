-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Plans that restore a backup into a volume (§5.4 of docs/06-storage.md) name the volume they
-- operate on, as every plan on a volume does.

ALTER TABLE plan DROP CONSTRAINT plan_kind_check;
ALTER TABLE plan ADD CONSTRAINT plan_kind_check
    CHECK (kind IN ('intent', 'power', 'retire', 'migrate', 'delete_volume', 'snapshot_volume',
                    'rollback_volume', 'delete_snapshot', 'backup_volume', 'restore_volume'));
ALTER TABLE plan DROP CONSTRAINT plan_to_volume_check;
ALTER TABLE plan ADD CONSTRAINT plan_to_volume_check
    CHECK ((kind IN ('delete_volume', 'snapshot_volume', 'rollback_volume', 'delete_snapshot',
                     'backup_volume', 'restore_volume'))
           = (to_volume IS NOT NULL));
