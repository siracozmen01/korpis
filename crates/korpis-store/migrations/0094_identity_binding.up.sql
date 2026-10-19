-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Identity bindings (§5 of docs/08-identity.md): which identity provider vouches for an
-- external person or set, how often that is re-checked, and what a set's members receive. A
-- binding is declared in the organization its provider is installed into, and a subject is bound
-- once while live, so whether a subject vouches has one answer. A retired binding is kept: the
-- effects that name it outlive it, and its subject may be bound again.

CREATE TABLE identity_binding (
    id                  uuid PRIMARY KEY,
    organization_id     uuid NOT NULL REFERENCES organization (id),
    provider            text NOT NULL,
    -- The principal key of the external identity it binds.
    subject             text NOT NULL,
    is_set              boolean NOT NULL,
    origin              text NOT NULL CHECK (origin IN ('operator', 'first_use')),
    template_id         uuid,
    template_version    bigint,
    require_membership  jsonb NOT NULL,
    grace_ms            bigint NOT NULL CHECK (grace_ms >= 0),
    reverify_ms         bigint NOT NULL CHECK (reverify_ms > 0),
    allows_irreversible boolean NOT NULL,
    state               text NOT NULL CHECK (state IN ('verified', 'unverified')),
    last_verified_at    timestamptz,
    version             bigint NOT NULL CHECK (version >= 1),
    made_by             text NOT NULL,
    made_at             timestamptz NOT NULL,
    retired_at          timestamptz,
    FOREIGN KEY (template_id, template_version)
        REFERENCES grant_template_version (template_id, version),
    CHECK ((template_id IS NULL) = (template_version IS NULL)),
    CHECK (is_set OR (template_id IS NULL AND NOT allows_irreversible)),
    CHECK (is_set = false OR origin = 'operator')
);
CREATE UNIQUE INDEX identity_binding_subject ON identity_binding (subject)
    WHERE retired_at IS NULL;
CREATE INDEX identity_binding_organization ON identity_binding (organization_id, id);

GRANT SELECT, INSERT, UPDATE ON identity_binding TO korpis_app;
