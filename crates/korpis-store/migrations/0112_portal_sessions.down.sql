-- SPDX-License-Identifier: AGPL-3.0-or-later

ALTER TABLE webauthn_challenge DROP COLUMN portal_id;
DROP INDEX passkey_portal;
ALTER TABLE passkey DROP COLUMN portal_id;
DROP INDEX user_session_portal;
ALTER TABLE user_session DROP COLUMN portal_id;
