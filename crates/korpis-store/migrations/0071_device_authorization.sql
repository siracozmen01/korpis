-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The CLI's sign-in, the OAuth device authorization grant (RFC 8628; §9.4 of
-- docs/08-identity.md). A device authorization is kept by the digest of its device code, which
-- only the CLI that asked holds, and by its user code, which the user types into the panel. It is
-- approved or denied by a signed-in account, and spent by the poll that finds it approved, which
-- begins the CLI's session and deletes it; an expired one is forgotten.

CREATE TABLE device_authorization (
    id              uuid PRIMARY KEY,
    device_hash     bytea NOT NULL UNIQUE CHECK (octet_length(device_hash) = 32),
    user_code       text NOT NULL UNIQUE CHECK (user_code ~ '^[BCDFGHJKLMNPQRSTVWXZ]{8}$'),
    created_at      timestamptz NOT NULL,
    expires_at      timestamptz NOT NULL,
    interval_secs   integer NOT NULL CHECK (interval_secs > 0),
    last_polled_at  timestamptz,
    user_agent      text NOT NULL CHECK (char_length(user_agent) <= 256),
    source          text NOT NULL,
    decided_by      uuid REFERENCES account (id) ON DELETE CASCADE,
    decided_at      timestamptz,
    approved        boolean,
    CHECK ((decided_by IS NULL) = (decided_at IS NULL) AND (decided_by IS NULL) = (approved IS NULL))
);
CREATE INDEX device_authorization_expiry ON device_authorization (expires_at);

GRANT SELECT, INSERT, UPDATE, DELETE ON device_authorization TO korpis_app;
