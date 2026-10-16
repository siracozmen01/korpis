-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Accounts and their factors (§9.6 of docs/08-identity.md). An account is the subject
-- `user:<id>`; it is created holding the grant it was invited with, and gains its first factor by
-- redeeming the invitation. Every secret here is a digest or sealed: a passkey's public key, a
-- password's Argon2id hash, a TOTP secret sealed by the vault, recovery codes' digests.

CREATE TABLE account (
    id            uuid PRIMARY KEY,
    display_name  text NOT NULL CHECK (char_length(display_name) BETWEEN 1 AND 100),
    -- What a password sign-in names it by, chosen when its invitation is redeemed; lowercase.
    name          text UNIQUE CHECK (name ~ '^[a-z0-9][a-z0-9._-]{1,62}[a-z0-9]$'),
    -- The principal key of whoever invited it.
    created_by    text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    disabled_at   timestamptz
);

-- An invitation registers an account's first factor, once, before it expires. Only its digest is
-- kept.
CREATE TABLE invitation (
    id           uuid PRIMARY KEY,
    account_id   uuid NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    secret_hash  bytea NOT NULL UNIQUE,
    issued_by    text NOT NULL,
    issued_at    timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    -- Redeemable only by registering a passkey: the bootstrap invitation and recover-root's.
    passkey_only boolean NOT NULL DEFAULT false,
    redeemed_at  timestamptz
);
CREATE INDEX invitation_account ON invitation (account_id);

CREATE TABLE passkey (
    credential_id    bytea PRIMARY KEY CHECK (octet_length(credential_id) BETWEEN 1 AND 1023),
    account_id       uuid NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    -- The COSE key, as the authenticator encoded it.
    public_key       bytea NOT NULL,
    -- Its COSE algorithm: -7, -8, or -257.
    algorithm        integer NOT NULL CHECK (algorithm IN (-7, -8, -257)),
    sign_count       bigint NOT NULL CHECK (sign_count >= 0),
    aaguid           bytea NOT NULL CHECK (octet_length(aaguid) = 16),
    backup_eligible  boolean NOT NULL,
    label            text NOT NULL DEFAULT '',
    created_at       timestamptz NOT NULL DEFAULT now(),
    last_used_at     timestamptz
);
CREATE INDEX passkey_account ON passkey (account_id);

CREATE TABLE totp_factor (
    account_id  uuid PRIMARY KEY REFERENCES account (id) ON DELETE CASCADE,
    sealed_by   text NOT NULL,
    sealed      bytea NOT NULL,
    -- The step of the code accepted last; a code is accepted only for a later step.
    last_step   bigint,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE password_factor (
    account_id  uuid PRIMARY KEY REFERENCES account (id) ON DELETE CASCADE,
    -- The Argon2id hash as a PHC string.
    phc         text NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE recovery_code (
    account_id  uuid NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    digest      bytea NOT NULL CHECK (octet_length(digest) = 32),
    used_at     timestamptz,
    PRIMARY KEY (account_id, digest)
);

-- A WebAuthn challenge, issued for one ceremony and taken by the ceremony that answers it. Not
-- logged: a crash forgets an open ceremony, and its user starts it again.
CREATE UNLOGGED TABLE webauthn_challenge (
    id             uuid PRIMARY KEY,
    -- `register` or `sign_in`.
    purpose        text NOT NULL CHECK (purpose IN ('register', 'sign_in')),
    challenge      bytea NOT NULL CHECK (octet_length(challenge) = 32),
    -- The account a registration adds a passkey to, or the invitation it redeems.
    account_id     uuid REFERENCES account (id) ON DELETE CASCADE,
    invitation_id  uuid REFERENCES invitation (id) ON DELETE CASCADE,
    expires_at     timestamptz NOT NULL
);
CREATE INDEX webauthn_challenge_expiry ON webauthn_challenge (expires_at);

GRANT SELECT, INSERT, UPDATE ON account TO korpis_app;
GRANT SELECT, INSERT, UPDATE ON invitation TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON passkey TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON totp_factor TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON password_factor TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON recovery_code TO korpis_app;
-- UPDATE only for the row locks an insert takes on the expired challenges it forgets, so that two
-- inserts forget different ones instead of waiting on each other.
GRANT SELECT, INSERT, UPDATE, DELETE ON webauthn_challenge TO korpis_app;
