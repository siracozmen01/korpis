-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Signing in through an identity provider (§9.7 of docs/08-identity.md). A provider says when it
-- registers whether it signs people in, and only one that does is offered or asked to. A sign-in
-- begun waits ten minutes for its person to return, kept by its state's digest, so the store's
-- contents begin no one's sign-in, with the nonce its ID token must carry and the PKCE challenge
-- of the verifier the panel keeps; it is spent once when the person returns, and forgotten when
-- it expires unspent.

ALTER TABLE provider_registration
    ADD COLUMN sign_in boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT provider_registration_sign_in CHECK (NOT sign_in OR type = 'identity');

CREATE TABLE pending_sign_in (
    state_digest     bytea PRIMARY KEY CHECK (length(state_digest) = 32),
    organization_id  uuid NOT NULL REFERENCES organization (id),
    provider         text NOT NULL,
    nonce            text NOT NULL,
    code_challenge   text NOT NULL,
    begun_at         timestamptz NOT NULL,
    expires_at       timestamptz NOT NULL
);
CREATE INDEX pending_sign_in_expires ON pending_sign_in (expires_at);

GRANT SELECT, INSERT, DELETE ON pending_sign_in TO korpis_app;
