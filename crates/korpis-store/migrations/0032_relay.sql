-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Relays that run as their own process (§3.1 of docs/02-architecture.md), enrolled like nodes: an
-- operator creates a one-time token bound to the relay's name and URL, and the relay redeems it
-- once with the public half of its identity key. The token is an ordinary Token subject holding a
-- grant of node.enroll over the root organization with max_uses 1, as a node's is; this row binds
-- to it what the relay will be.

CREATE TABLE relay (
    id                      uuid PRIMARY KEY,
    name                    text NOT NULL UNIQUE,
    url                     text NOT NULL UNIQUE,
    -- Ed25519, raw: what the relay signs its requests to the control plane with.
    public_key              bytea NOT NULL UNIQUE CHECK (octet_length(public_key) = 32),
    joined_at               timestamptz NOT NULL,
    certificate_expires_at  timestamptz,
    removed_at              timestamptz
);

CREATE TABLE relay_enrollment (
    token_id     uuid PRIMARY KEY REFERENCES api_token (id),
    grant_id     uuid NOT NULL REFERENCES grants (id),
    organization_id uuid NOT NULL REFERENCES organization (id),
    relay_name   text NOT NULL,
    relay_url    text NOT NULL,
    created_by   text NOT NULL,
    created_at   timestamptz NOT NULL,
    expires_at   timestamptz NOT NULL,
    relay_id     uuid UNIQUE REFERENCES relay (id),
    joined_at    timestamptz,
    CHECK ((relay_id IS NULL) = (joined_at IS NULL))
);

GRANT SELECT, INSERT, UPDATE ON relay TO korpis_app;
GRANT SELECT, INSERT, UPDATE ON relay_enrollment TO korpis_app;
