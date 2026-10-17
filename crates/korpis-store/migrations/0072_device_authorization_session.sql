-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The session that approved a device authorization (Finding 75 of docs/23-walkthroughs.md). An
-- approval is honoured by the CLI's poll only while that session lasts, so signing it out, ending
-- it, or ending every other session withdraws an approval the CLI has not yet collected.

ALTER TABLE device_authorization
    ADD COLUMN decided_session uuid REFERENCES user_session (id) ON DELETE SET NULL;
