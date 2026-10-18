-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A restore's settling window, pending until the control plane starts (§8 of docs/03-state.md).
-- The restore command fences the epochs while the control plane is stopped, and a window opened
-- then would run out before an operator started it, with no agent ever asked to present. So the
-- restore records how long the window is, and the first start opens it.

ALTER TABLE epoch_state ADD COLUMN settle_pending_ms bigint CHECK (settle_pending_ms > 0);
