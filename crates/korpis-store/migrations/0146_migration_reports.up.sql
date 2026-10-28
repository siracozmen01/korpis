-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- What each side of a migration reported (§8.1 of docs/05-scheduling.md): the last phase each
-- node reported done, which the control plane advances both nodes on, and, for each volume, each
-- side's manifest in runs of entries, each run's first path and digest, from which the first path
-- where two copies differ is named. The placement's move at cutover is recorded too, so the
-- reconciler that makes it does so once.

ALTER TABLE migration
    ADD COLUMN source_done text CHECK (source_done IN ('prepare', 'replicate', 'quiesce',
        'final_sync', 'verify', 'cutover', 'release')),
    ADD COLUMN destination_done text CHECK (destination_done IN ('prepare', 'replicate',
        'quiesce', 'final_sync', 'verify', 'cutover', 'release')),
    ADD COLUMN moved_at timestamptz;

ALTER TABLE migration_volume
    ADD COLUMN source_chunks jsonb,
    ADD COLUMN destination_chunks jsonb;
