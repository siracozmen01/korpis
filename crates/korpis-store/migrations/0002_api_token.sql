-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- API tokens: the credential of a Token subject (§3.2 of docs/01-model.md), for the CLI in
-- Phase 0 and for CI pipelines and integrations always. The secret is shown once and stored only
-- as a SHA-256 digest: it is 256 bits of randomness, so a slow hash would protect nothing.

CREATE TABLE api_token (
    id           uuid PRIMARY KEY,
    name         text NOT NULL,
    secret_hash  bytea NOT NULL UNIQUE,
    -- The principal key of whoever created it (§1 of `principal.rs` in korpis-core).
    created_by   text NOT NULL,
    created      timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz,
    revoked_at   timestamptz,
    last_used    timestamptz
);

GRANT SELECT, INSERT, UPDATE ON api_token TO korpis_app;
