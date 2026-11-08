-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Extensions' actions and an operator's rule over extensions.
--
-- An extension action's invocations while they run. The token the extension acts with for the
-- invoker is kept as its digest; it is accepted only while its invocation is unfinished and
-- unexpired, beside the credential of the installation invoked, and only on the objects recorded
-- here, which are the ones the invoker was authorized on.

CREATE TABLE extension_invocation (
    id             uuid PRIMARY KEY,
    extension_id   uuid NOT NULL REFERENCES extension (id),
    action         text NOT NULL,
    invoker        text NOT NULL,
    -- [{"kind": "workload", "id": "<uuid>"}, ...]
    resources      jsonb NOT NULL,
    secret_digest  bytea NOT NULL UNIQUE,
    created_at     timestamptz NOT NULL,
    expires_at     timestamptz NOT NULL,
    finished_at    timestamptz
);

CREATE INDEX extension_invocation_expires ON extension_invocation (expires_at);

GRANT SELECT, INSERT, UPDATE, DELETE ON extension_invocation TO korpis_app;

-- The invocation an effect was made under, when an extension acted for whoever invoked it. No
-- foreign key: effects outlive the invocations they name, which are forgotten once finished.
ALTER TABLE effect ADD COLUMN invocation_id uuid;

-- A root organization's rule over the extensions installed beneath it: the registries their
-- artifacts may come from, the publishers one of whom must have signed them, and whether anyone
-- without extension.install over the root may install one.
CREATE TABLE extension_policy (
    organization_id  uuid PRIMARY KEY REFERENCES organization (id),
    registries       text[] NOT NULL,
    -- [{"name": ..., "public_key_pem": ..., "fingerprint": "sha256:..."}, ...]
    publishers       jsonb NOT NULL,
    tenants_install  boolean NOT NULL,
    version          bigint NOT NULL CHECK (version > 0),
    set_by           text NOT NULL,
    set_at           timestamptz NOT NULL
);

GRANT SELECT, INSERT, UPDATE ON extension_policy TO korpis_app;
