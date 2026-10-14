-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Prune plans (§5.3 of docs/06-storage.md) name the repository whose backups of their workload
-- they prune. Every access a job or a reader is given to a workload's prefix in a repository is
-- recorded before it is asked of the target, with when it ends, so a sweep can tell that no writer it does not know of
-- remains (a job whose plan ended while it ran), and a prune that no reader is still reading a
-- backup it would forget (§5.5). A row is written before the grant is asked, so access granted
-- is always recorded, and access that failed to be granted is recorded too, which only delays a
-- sweep.

ALTER TABLE plan ADD COLUMN to_repository uuid REFERENCES repository (id);
ALTER TABLE plan DROP CONSTRAINT plan_kind_check;
ALTER TABLE plan ADD CONSTRAINT plan_kind_check
    CHECK (kind IN ('intent', 'power', 'retire', 'migrate', 'delete_volume', 'snapshot_volume',
                    'rollback_volume', 'delete_snapshot', 'backup_volume', 'restore_volume',
                    'prune_backups'));
ALTER TABLE plan ADD CONSTRAINT plan_to_repository_check
    CHECK ((kind = 'prune_backups') = (to_repository IS NOT NULL));

CREATE TABLE backup_access (
    id             uuid PRIMARY KEY,
    repository_id  uuid NOT NULL REFERENCES repository (id),
    workload_id    uuid NOT NULL REFERENCES workload (id),
    -- The backup read, for read access; NULL for a writer.
    backup_id      uuid REFERENCES backup (id),
    mode           text NOT NULL CHECK (mode IN ('read', 'write')),
    granted_at     timestamptz NOT NULL,
    expires_at     timestamptz NOT NULL,
    CONSTRAINT backup_access_reads_a_backup CHECK ((mode = 'read') = (backup_id IS NOT NULL))
);
CREATE INDEX backup_access_live ON backup_access (repository_id, workload_id, expires_at);

GRANT SELECT, INSERT, DELETE ON backup_access TO korpis_app;

-- What a prune found that no backup it keeps references (§5.5 of docs/06-storage.md): names
-- relative to the workload's prefix, deleted by a sweep only once a grace period longer than any
-- job's access has passed since they were marked, and only if they are still unreferenced then.
CREATE TABLE prune_mark (
    repository_id  uuid NOT NULL REFERENCES repository (id),
    workload_id    uuid NOT NULL REFERENCES workload (id),
    name           text NOT NULL,
    plan_id        uuid NOT NULL REFERENCES plan (id),
    marked_at      timestamptz NOT NULL,
    PRIMARY KEY (repository_id, workload_id, name)
);

-- The sweep holding a workload's prefix in a repository, which a backup waits for: the one
-- writer per prefix of §5.5, taken by the sweep and checked by every backup proposed.
ALTER TABLE workload_key ADD COLUMN sweeping_job uuid;

GRANT SELECT, INSERT, DELETE ON prune_mark TO korpis_app;
GRANT UPDATE (sweeping_job) ON workload_key TO korpis_app;
