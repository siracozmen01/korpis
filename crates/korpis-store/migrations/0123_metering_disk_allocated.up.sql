-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Whether a metering record's disk_bytes counts what block devices' images have allocated rather
-- than what filesystems hold (§3.1 of docs/06-storage.md): a microVM's disk usage is a different
-- number from a container's, and is labelled as such. Every record before it counted files.

ALTER TABLE metering_record ADD COLUMN disk_allocated boolean NOT NULL DEFAULT false;
