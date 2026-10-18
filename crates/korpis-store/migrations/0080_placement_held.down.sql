-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0080. A held placement is sent again.

ALTER TABLE placement DROP COLUMN held_at;
