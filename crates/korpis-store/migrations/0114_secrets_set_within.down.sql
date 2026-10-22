-- SPDX-License-Identifier: AGPL-3.0-or-later

ALTER TABLE user_session DROP COLUMN within_id;
ALTER TABLE account
    DROP COLUMN recovery_codes_set_within,
    DROP COLUMN totp_set_within,
    DROP COLUMN password_set_within;
