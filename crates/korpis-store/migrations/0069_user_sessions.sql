-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- User sessions (§9.4 of docs/08-identity.md). A session is kept by the digest of its current
-- refresh credential; each exchange rotates it, and the digests rotated away are kept for the
-- session's life, so a rotated credential presented again is known for the copy it must be and
-- ends the session. Ending a session deletes it and, with it, every digest it rotated.

CREATE TABLE user_session (
    id                uuid PRIMARY KEY,
    account_id        uuid NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    kind              text NOT NULL CHECK (kind IN ('web', 'cli')),
    refresh_hash      bytea NOT NULL UNIQUE CHECK (octet_length(refresh_hash) = 32),
    created_at        timestamptz NOT NULL,
    last_used_at      timestamptz NOT NULL,
    authenticated_at  timestamptz NOT NULL,
    expires_at        timestamptz NOT NULL,
    user_agent        text NOT NULL CHECK (char_length(user_agent) <= 256),
    source            text NOT NULL
);
CREATE INDEX user_session_account ON user_session (account_id);
CREATE INDEX user_session_expiry ON user_session (expires_at);
CREATE INDEX user_session_last_used ON user_session (last_used_at);

CREATE TABLE user_session_rotated (
    refresh_hash  bytea PRIMARY KEY CHECK (octet_length(refresh_hash) = 32),
    session_id    uuid NOT NULL REFERENCES user_session (id) ON DELETE CASCADE
);
CREATE INDEX user_session_rotated_session ON user_session_rotated (session_id);

GRANT SELECT, INSERT, UPDATE, DELETE ON user_session TO korpis_app;
GRANT SELECT, INSERT, DELETE ON user_session_rotated TO korpis_app;
