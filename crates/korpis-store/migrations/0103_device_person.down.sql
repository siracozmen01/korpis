-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0103: authorizations a person decided are abandoned, and only an account
-- decides again.

DELETE FROM device_authorization WHERE decided_binding IS NOT NULL;
ALTER TABLE device_authorization
    DROP CONSTRAINT device_authorization_decided,
    DROP COLUMN decided_binding,
    ADD CONSTRAINT device_authorization_check CHECK (
        (decided_by IS NULL) = (decided_at IS NULL) AND (decided_by IS NULL) = (approved IS NULL));
