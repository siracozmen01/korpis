-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- For a block device's backup, the filesystem its image holds, as the backup's record in the
-- repository says (§5.4 of docs/06-storage.md): its files are read inside a guest, never by the
-- host or the control plane. Every backup before it was of a volume of files, or says nothing.

ALTER TABLE backup ADD COLUMN image_filesystem text;
