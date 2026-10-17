-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0077.

ALTER TABLE epoch_state DROP COLUMN bucket_tokens, DROP COLUMN bucket_at;
