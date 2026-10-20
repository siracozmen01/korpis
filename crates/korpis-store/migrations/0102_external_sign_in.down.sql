-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0102: sign-ins begun are abandoned, and providers no longer say they sign in.

DROP TABLE pending_sign_in;
ALTER TABLE provider_registration
    DROP CONSTRAINT provider_registration_sign_in,
    DROP COLUMN sign_in;
