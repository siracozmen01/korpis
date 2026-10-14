-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Backup repositories, the keys their backups are sealed under, and backups (§5 of
-- docs/06-storage.md). A repository's key-encryption key is stored only sealed by the control
-- plane's backup key, and each workload's data key only sealed by its repository's key-encryption
-- key, so this database alone opens no backup. A backup is a record of what a node reported; a
-- pruned one is marked, never removed.

CREATE TABLE repository (
    id               uuid PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    -- The project it is scoped to; the whole organization when null.
    project_id       uuid REFERENCES project (id),
    name             text NOT NULL,
    -- The organization's backup target provider, by name.
    provider_name    text NOT NULL,
    -- Its place in the target, a relative prefix ending in '/'.
    prefix           text NOT NULL CHECK (prefix LIKE '%/'),
    key_custody      text NOT NULL CHECK (key_custody IN ('operator', 'tenant')),
    -- The key-encryption key, sealed, and the key that sealed it.
    kek_sealed       bytea NOT NULL,
    kek_sealed_by    text NOT NULL,
    keep_last        integer NOT NULL CHECK (keep_last >= 0),
    keep_hourly      integer NOT NULL CHECK (keep_hourly >= 0),
    keep_daily       integer NOT NULL CHECK (keep_daily >= 0),
    keep_weekly      integer NOT NULL CHECK (keep_weekly >= 0),
    keep_monthly     integer NOT NULL CHECK (keep_monthly >= 0),
    keep_yearly      integer NOT NULL CHECK (keep_yearly >= 0),
    keep_failed_s    bigint NOT NULL CHECK (keep_failed_s >= 0),
    min_retention_s  bigint NOT NULL CHECK (min_retention_s >= 0),
    created_at       timestamptz NOT NULL,
    deleted_at       timestamptz
);

-- A name names one live repository of its organization.
CREATE UNIQUE INDEX repository_live_name ON repository (organization_id, name)
    WHERE deleted_at IS NULL;

-- One data key per workload per repository, generated at its first backup there.
CREATE TABLE workload_key (
    repository_id  uuid NOT NULL REFERENCES repository (id),
    workload_id    uuid NOT NULL REFERENCES workload (id),
    dek_sealed     bytea NOT NULL,
    created_at     timestamptz NOT NULL,
    PRIMARY KEY (repository_id, workload_id)
);

CREATE TABLE backup (
    id               uuid PRIMARY KEY,
    repository_id    uuid NOT NULL REFERENCES repository (id),
    workload_id      uuid NOT NULL REFERENCES workload (id),
    volume_id        uuid NOT NULL REFERENCES volume (id),
    plan_id          uuid NOT NULL REFERENCES plan (id),
    parent_id        uuid REFERENCES backup (id),
    status           text NOT NULL CHECK (status IN ('pending', 'succeeded', 'failed')),
    consistent       boolean NOT NULL,
    files            bigint NOT NULL DEFAULT 0,
    directories      bigint NOT NULL DEFAULT 0,
    bytes            bigint NOT NULL DEFAULT 0,
    chunks_new       bigint NOT NULL DEFAULT 0,
    bytes_new        bigint NOT NULL DEFAULT 0,
    stored_bytes     bigint NOT NULL DEFAULT 0,
    files_unchanged  bigint NOT NULL DEFAULT 0,
    skipped          text[] NOT NULL DEFAULT '{}',
    error            text,
    created_at       timestamptz NOT NULL,
    finished_at      timestamptz,
    deleted_at       timestamptz
);

CREATE INDEX backup_workload ON backup (workload_id, id);
CREATE INDEX backup_repository ON backup (repository_id, id);

GRANT SELECT, INSERT, UPDATE ON repository TO korpis_app;
GRANT SELECT, INSERT ON workload_key TO korpis_app;
GRANT SELECT, INSERT, UPDATE ON backup TO korpis_app;

-- The organization that owns an object, now for repositories and volumes too.
CREATE OR REPLACE FUNCTION korpis_object_organization(kind text, object uuid) RETURNS uuid
LANGUAGE sql STABLE AS $$
    SELECT CASE kind
        WHEN 'organization' THEN object
        WHEN 'project' THEN (SELECT organization_id FROM project WHERE id = object)
        WHEN 'workload' THEN (SELECT organization_id FROM workload WHERE id = object)
        WHEN 'node' THEN (SELECT organization_id FROM node WHERE id = object)
        WHEN 'volume' THEN (SELECT p.organization_id FROM volume v
                              JOIN project p ON p.id = v.project_id WHERE v.id = object)
        WHEN 'repository' THEN (SELECT organization_id FROM repository WHERE id = object)
    END
$$;
