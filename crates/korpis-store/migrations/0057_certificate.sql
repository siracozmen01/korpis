-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Certificates the ingress serves (§3.2 of docs/07-networking.md, §3.6 of docs/01-model.md). The
-- private key is stored only sealed, by the vault key `sealed_by` names, bound to the
-- certificate's identifier. A route names the certificate its endpoint serves with; the reference
-- is what keeps a certificate in use from being deleted, so the refusal holds however the delete
-- and the claim interleave.

CREATE TABLE certificate (
    id               uuid PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    names            text[] NOT NULL,
    source           text NOT NULL CHECK (source IN ('acme', 'provided')),
    -- PEM, leaf first; empty while pending.
    chain            text NOT NULL,
    key_sealed       bytea,
    sealed_by        text,
    not_before       timestamptz,
    not_after        timestamptz,
    state            text NOT NULL CHECK (state IN ('pending', 'issued', 'failed')),
    reason           text,
    renew_after      timestamptz,
    -- Raised whenever the chain and key change, so a node serving it fetches it again.
    revision         bigint NOT NULL,
    created_at       timestamptz NOT NULL,
    -- An issued certificate whose renewal fails keeps serving, its reason recorded beside it.
    CHECK (state <> 'issued' OR (key_sealed IS NOT NULL AND sealed_by IS NOT NULL
                                 AND not_before IS NOT NULL AND not_after IS NOT NULL)),
    CHECK (state <> 'failed' OR reason IS NOT NULL)
);

CREATE INDEX certificate_organization ON certificate (organization_id);

ALTER TABLE route ADD COLUMN certificate_id uuid REFERENCES certificate (id);
CREATE INDEX route_certificate ON route (certificate_id) WHERE certificate_id IS NOT NULL;

GRANT SELECT, INSERT, UPDATE, DELETE ON certificate TO korpis_app;
