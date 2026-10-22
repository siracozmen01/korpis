-- SPDX-License-Identifier: AGPL-3.0-or-later

-- Where an account's shared secrets were set, and how far a session reaches (§9.8 of
-- docs/08-identity.md, Finding 133 of docs/23-walkthroughs.md). Each of the password, the TOTP
-- secret, and the recovery codes records the organization beneath which the session that set it
-- was confined, none when it was not; a session records the organization beneath which it acts,
-- which its access tokens name. Neither references the organization: one deleted still confines
-- what recorded it, reaching nothing, where a reference would have let it go.

ALTER TABLE account
    ADD COLUMN password_set_within uuid,
    ADD COLUMN totp_set_within uuid,
    ADD COLUMN recovery_codes_set_within uuid;

ALTER TABLE user_session ADD COLUMN within_id uuid;
UPDATE user_session s SET within_id = p.organization_id FROM portal p WHERE p.id = s.portal_id;
