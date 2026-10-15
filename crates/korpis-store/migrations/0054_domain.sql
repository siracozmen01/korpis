-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Domains organizations hold (§3.2 of docs/07-networking.md, §3.6 of docs/01-model.md). That two
-- organizations' verified domains do not overlap is not a property an index can hold, since
-- overlap is one name lying beneath another; verification takes the domains lock and checks it in
-- the same transaction, so two verifications that would overlap are serialized and the second is
-- refused.

CREATE TABLE domain (
    id               uuid PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    name             text NOT NULL,
    verification     text NOT NULL CHECK (verification IN ('dns_txt', 'delegated', 'operator')),
    -- What the TXT record at _korpis-verify.<name> must hold, for dns_txt.
    token            text NOT NULL,
    -- For delegated: the domain it lies beneath, whose holder delegated it.
    parent_id        uuid REFERENCES domain (id),
    verified_at      timestamptz,
    created_at       timestamptz NOT NULL,
    CHECK ((verification = 'delegated') = (parent_id IS NOT NULL)),
    UNIQUE (organization_id, name)
);

CREATE INDEX domain_verified_name ON domain (name) WHERE verified_at IS NOT NULL;

GRANT SELECT, INSERT, UPDATE, DELETE ON domain TO korpis_app;
