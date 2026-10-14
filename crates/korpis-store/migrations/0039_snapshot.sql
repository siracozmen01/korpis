-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Snapshots (§4 of docs/06-storage.md), kept from what nodes report, never removed: a deleted
-- snapshot is marked. And the plans that take, roll back to, and delete them, which like a plan
-- deleting a retained volume name the volume they operate on.

CREATE TABLE snapshot (
    id          uuid PRIMARY KEY,
    volume_id   uuid NOT NULL REFERENCES volume (id),
    name        text NOT NULL,
    created_at  timestamptz NOT NULL,
    deleted_at  timestamptz
);

-- A name names one live snapshot of its volume.
CREATE UNIQUE INDEX snapshot_live_name ON snapshot (volume_id, name) WHERE deleted_at IS NULL;

GRANT SELECT, INSERT, UPDATE ON snapshot TO korpis_app;

ALTER TABLE plan DROP CONSTRAINT plan_kind_check;
ALTER TABLE plan ADD CONSTRAINT plan_kind_check
    CHECK (kind IN ('intent', 'power', 'retire', 'migrate', 'delete_volume', 'snapshot_volume',
                    'rollback_volume', 'delete_snapshot'));
ALTER TABLE plan DROP CONSTRAINT plan_to_volume_check;
ALTER TABLE plan ADD CONSTRAINT plan_to_volume_check
    CHECK ((kind IN ('delete_volume', 'snapshot_volume', 'rollback_volume', 'delete_snapshot'))
           = (to_volume IS NOT NULL));
