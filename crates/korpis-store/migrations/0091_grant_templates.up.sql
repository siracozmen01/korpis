-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Grant templates (§4 of docs/08-identity.md): a named, versioned request for a grant, owned by
-- an organization and offered to its subtree. The head names the current version and whether the
-- template is retired; every version is kept whole beside it, because a grant issued from one
-- names it as provenance and that name must say exactly what was issued. A name is unique among
-- an organization's live templates, so a retired template's name is free to reuse.

CREATE TABLE grant_template (
    id               uuid PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    name             text NOT NULL,
    version          bigint NOT NULL CHECK (version >= 1),
    retired_at       timestamptz
);
CREATE UNIQUE INDEX grant_template_name ON grant_template (organization_id, name)
    WHERE retired_at IS NULL;

CREATE TABLE grant_template_version (
    template_id  uuid NOT NULL REFERENCES grant_template (id),
    version      bigint NOT NULL CHECK (version >= 1),
    name         text NOT NULL,
    body         jsonb NOT NULL,
    made_by      text NOT NULL,
    made_at      timestamptz NOT NULL,
    PRIMARY KEY (template_id, version)
);

GRANT SELECT, INSERT, UPDATE ON grant_template TO korpis_app;
GRANT SELECT, INSERT ON grant_template_version TO korpis_app;
