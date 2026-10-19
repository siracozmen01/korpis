-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Secrets every control plane of an installation shares, made once by whichever starts first.
-- The first is the key page tokens are sealed under (§8 of docs/10-api.md): a token names a row
-- the caller may not see when a page stops reading before it is full, so a token is opaque, and
-- one control plane opens what another sealed. A secret here is never shown through the API; a
-- reader of this database already reads every row a token could name.

CREATE TABLE server_secret (
    name        text PRIMARY KEY,
    secret      bytea NOT NULL CHECK (octet_length(secret) = 32),
    created_at  timestamptz NOT NULL DEFAULT now()
);

GRANT SELECT, INSERT ON server_secret TO korpis_app;
