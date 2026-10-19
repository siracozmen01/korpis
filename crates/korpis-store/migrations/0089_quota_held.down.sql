-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0089.

DROP TABLE workload_held;
ALTER TABLE quota_entry DROP COLUMN held;
