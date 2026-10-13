-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The nonces a relay's signed requests carried, kept for twice the auth skew window so a request
-- is never accepted twice (§3.1 of docs/02-architecture.md), as a node's are.

CREATE TABLE relay_auth_nonce (
    relay_id  uuid NOT NULL REFERENCES relay (id),
    nonce     bytea NOT NULL,
    seen_at   timestamptz NOT NULL,
    PRIMARY KEY (relay_id, nonce)
);

GRANT SELECT, INSERT, DELETE ON relay_auth_nonce TO korpis_app;
