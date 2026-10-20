-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0095.

DROP INDEX identity_binding_due;
ALTER TABLE identity_binding DROP COLUMN verifications;
ALTER TABLE identity_binding DROP COLUMN verify_due_at;
