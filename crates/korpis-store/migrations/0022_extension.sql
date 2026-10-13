-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Extension installations (§4 of docs/16-extensions.md). An installation is the subject
-- extension:<id> its grants are issued to; its grants are found by that subject key, and what
-- vouched for its artifact is in its workload's intent, so neither is repeated here. A row is
-- kept after uninstalling, with the time, because effects name the subject for as long as the
-- audit record is kept.

CREATE TABLE extension (
    id               uuid PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    -- Installed into one project rather than the whole organization.
    project_id       uuid REFERENCES project (id),
    -- The document's metadata.name and metadata.version.
    name             text NOT NULL,
    version          text NOT NULL,
    api_version      text NOT NULL,
    -- The workload it runs as; one installation per workload, ever.
    workload_id      uuid NOT NULL UNIQUE REFERENCES workload (id),
    artifact_name    text NOT NULL,
    artifact_digest  text NOT NULL REFERENCES recipe (digest),
    installed_by     text NOT NULL,
    installed_at     timestamptz NOT NULL,
    uninstalled_at   timestamptz CHECK (uninstalled_at IS NULL OR uninstalled_at >= installed_at)
);
-- One installation of a name per organization and project at a time.
CREATE UNIQUE INDEX extension_installed_name ON extension
    (organization_id, coalesce(project_id, '00000000-0000-0000-0000-000000000000'), name)
    WHERE uninstalled_at IS NULL;
CREATE INDEX extension_organization ON extension (organization_id, id);

GRANT SELECT, INSERT, UPDATE ON extension TO korpis_app;
