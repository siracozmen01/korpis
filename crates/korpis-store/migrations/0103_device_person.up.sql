-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A device authorization decided by a person signed in through an identity provider (§9.7 of
-- docs/08-identity.md): `korpis login` is approved from their browser's session, and the CLI's
-- session is held by the same person. A decision names the account or the person's binding that
-- made it, exactly one of them, and is honoured only while that binding is not retired, as an
-- account's only while it may sign in.

ALTER TABLE device_authorization
    ADD COLUMN decided_binding uuid REFERENCES identity_binding (id) ON DELETE CASCADE,
    DROP CONSTRAINT device_authorization_check,
    ADD CONSTRAINT device_authorization_decided CHECK (
        (decided_by IS NULL OR decided_binding IS NULL)
        AND (decided_at IS NULL) = (decided_by IS NULL AND decided_binding IS NULL)
        AND (decided_at IS NULL) = (approved IS NULL));
