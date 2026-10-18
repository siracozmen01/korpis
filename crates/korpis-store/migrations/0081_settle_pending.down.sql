-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0081. A pending window is forgotten.

ALTER TABLE epoch_state DROP COLUMN settle_pending_ms;
