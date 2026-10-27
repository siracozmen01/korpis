-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A volume made from its replica on another node (§2.2 of docs/06-storage.md). The copy it leaves
-- on the node it failed over from is a volume of its own, retained and held by no workload, which
-- holds what was written there after the replica last received, so it is never collected and is
-- deleted, as any retained volume is, only by a plan (§7 there): it records the volume it diverged
-- from, since when, and when the replica the volume was made from received what it holds. And, on
-- the volume, the replica's snapshot it is being made from on its new node, which that node keeps
-- until it reports the volume made.

ALTER TABLE volume ADD COLUMN divergent_of uuid REFERENCES volume (id);
ALTER TABLE volume ADD COLUMN divergent_since timestamptz;
ALTER TABLE volume ADD COLUMN held_until timestamptz;
ALTER TABLE volume ADD COLUMN promoting uuid;
ALTER TABLE volume ADD CONSTRAINT volume_divergent CHECK (
    (divergent_of IS NULL) = (divergent_since IS NULL)
    AND (divergent_of IS NULL) = (held_until IS NULL));

CREATE INDEX volume_divergent_of ON volume (divergent_of) WHERE divergent_of IS NOT NULL;
