-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Providers (§3.3 and §5 of docs/16-extensions.md). An installation's `provides` entries are what
-- its streams may register, each name unique per type within the organization it serves. A
-- registration is one open `ProviderService.Serve` stream, held by one control-plane replica;
-- the replica keeps it seen while the stream is open, and a registration not seen for a while
-- belongs to a replica that is gone and is withdrawn by whichever replica notices.

CREATE TABLE extension_provides (
    extension_id     uuid NOT NULL REFERENCES extension (id),
    organization_id  uuid NOT NULL REFERENCES organization (id),
    type             text NOT NULL,
    name             text NOT NULL,
    PRIMARY KEY (extension_id, type, name),
    CONSTRAINT extension_provides_name_in_organization UNIQUE (organization_id, type, name)
);

CREATE TABLE provider_registration (
    id               uuid PRIMARY KEY,
    extension_id     uuid NOT NULL REFERENCES extension (id),
    organization_id  uuid NOT NULL REFERENCES organization (id),
    type             text NOT NULL,
    name             text NOT NULL,
    -- The provider's own version, for routing new calls to the newest (§9 of
    -- docs/16-extensions.md, question 1).
    version          text NOT NULL,
    concurrency      integer NOT NULL CHECK (concurrency > 0),
    -- The control-plane process holding the stream.
    replica          uuid NOT NULL,
    connected_at     timestamptz NOT NULL,
    seen_at          timestamptz NOT NULL
);
CREATE INDEX provider_registration_serves ON provider_registration (organization_id, type, name);
CREATE INDEX provider_registration_replica ON provider_registration (replica);

GRANT SELECT, INSERT, DELETE ON extension_provides TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON provider_registration TO korpis_app;
