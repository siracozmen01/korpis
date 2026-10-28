-- SPDX-License-Identifier: AGPL-3.0-or-later

ALTER TABLE migration_volume DROP COLUMN destination_chunks, DROP COLUMN source_chunks;
ALTER TABLE migration DROP COLUMN moved_at, DROP COLUMN destination_done, DROP COLUMN source_done;
