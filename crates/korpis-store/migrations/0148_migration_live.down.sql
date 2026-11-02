-- SPDX-License-Identifier: AGPL-3.0-or-later

DELETE FROM migration_volume
 WHERE volume_id IN (SELECT id FROM volume WHERE kind = 'state');
DELETE FROM volume WHERE kind = 'state';
ALTER TABLE volume DROP CONSTRAINT volume_kind_check;
ALTER TABLE volume ADD CONSTRAINT volume_kind_check CHECK (kind IN ('declared', 'stream'));
ALTER TABLE migration DROP COLUMN live;
