-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Stream offload (§4 of docs/14-streams.md, Finding 43 of docs/23-walkthroughs.md). A repository
-- that takes streams is where the console streams of the workloads it serves are offloaded: at
-- most one per organization as a whole and one per project, the project's taken first. A stream's
-- segments are kept beneath `<prefix>streams/<workload>/`, beside its backups, never beneath
-- them, so a prune of the backups, which removes what no backup references beneath
-- `workloads/<workload>/`, never reaches them.
--
-- Access is now recorded with what it reaches: a workload's backups or its streams. A sweep of
-- the backups waits for a writer of the backups only.

ALTER TABLE repository ADD COLUMN streams boolean NOT NULL DEFAULT false;
CREATE UNIQUE INDEX repository_streams ON repository
    (organization_id, COALESCE(project_id, '00000000-0000-0000-0000-000000000000'::uuid))
    WHERE streams AND deleted_at IS NULL;

ALTER TABLE backup_access ADD COLUMN area text NOT NULL DEFAULT 'backups'
    CHECK (area IN ('backups', 'streams'));
ALTER TABLE backup_access DROP CONSTRAINT backup_access_reads_a_backup;
ALTER TABLE backup_access ADD CONSTRAINT backup_access_reads_a_backup
    CHECK (area <> 'backups' OR (mode = 'read') = (backup_id IS NOT NULL));
