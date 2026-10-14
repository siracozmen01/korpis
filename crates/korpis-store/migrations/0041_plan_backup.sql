-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Plans that back a volume up (§5 of docs/06-storage.md) name the volume they operate on, as
-- every plan on a volume does.

ALTER TABLE plan DROP CONSTRAINT plan_kind_check;
ALTER TABLE plan ADD CONSTRAINT plan_kind_check
    CHECK (kind IN ('intent', 'power', 'retire', 'migrate', 'delete_volume', 'snapshot_volume',
                    'rollback_volume', 'delete_snapshot', 'backup_volume'));
ALTER TABLE plan DROP CONSTRAINT plan_to_volume_check;
ALTER TABLE plan ADD CONSTRAINT plan_to_volume_check
    CHECK ((kind IN ('delete_volume', 'snapshot_volume', 'rollback_volume', 'delete_snapshot',
                     'backup_volume'))
           = (to_volume IS NOT NULL));
