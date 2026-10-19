-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0090.

DROP TABLE placement_held;
ALTER TABLE quota_entry DROP COLUMN nodes;
