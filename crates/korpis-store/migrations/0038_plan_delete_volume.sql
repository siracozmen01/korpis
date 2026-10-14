-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A plan deleting one retained volume (§7 of docs/06-storage.md). Its workload is the one that
-- created the volume, retired or not; its node executes it, through a residual assignment when
-- the workload no longer runs there (§4.8 of docs/02-architecture.md).

ALTER TABLE plan DROP CONSTRAINT plan_kind_check;
ALTER TABLE plan ADD CONSTRAINT plan_kind_check
    CHECK (kind IN ('intent', 'power', 'retire', 'migrate', 'delete_volume'));
ALTER TABLE plan ADD COLUMN to_volume uuid REFERENCES volume (id);
ALTER TABLE plan ADD CONSTRAINT plan_to_volume_check
    CHECK ((kind = 'delete_volume') = (to_volume IS NOT NULL));

CREATE INDEX plan_to_volume ON plan (to_volume) WHERE to_volume IS NOT NULL;
