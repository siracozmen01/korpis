-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0150.

DROP TABLE extension_policy;
ALTER TABLE effect DROP COLUMN invocation_id;
DROP TABLE extension_invocation;
