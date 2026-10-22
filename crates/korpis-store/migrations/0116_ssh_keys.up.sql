-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The SSH public keys people register to reach their files over SFTP (§9.9 of
-- docs/08-identity.md). A key is named by its SHA-256 fingerprint, unique in the installation,
-- so a key belongs to one person; its wire form is checked by the application where it is set
-- and again where it is read. A key registered from a confined session records the organization
-- that session was confined beneath, and confines what it opens.

CREATE TABLE ssh_key (
    id               uuid PRIMARY KEY,
    account_id       uuid NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    kind             text NOT NULL
                     CHECK (kind IN ('ssh-ed25519', 'ecdsa-sha2-nistp256', 'ecdsa-sha2-nistp384',
                                     'ssh-rsa')),
    public_key       bytea NOT NULL CHECK (octet_length(public_key) BETWEEN 1 AND 4096),
    fingerprint      text NOT NULL UNIQUE CHECK (fingerprint LIKE 'SHA256:%'),
    label            text NOT NULL DEFAULT '' CHECK (char_length(label) <= 64),
    within_id        uuid REFERENCES organization (id),
    added_at         timestamptz NOT NULL,
    last_used_at     timestamptz
);
CREATE INDEX ssh_key_account ON ssh_key (account_id);

GRANT SELECT, INSERT, UPDATE, DELETE ON ssh_key TO korpis_app;
