-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A volume in a shared storage class (§2.1 of docs/06-storage.md) is in a store every node
-- offering that class reaches, not on one node's disk. It records its store, and its node_id is
-- then the node that last reported it, on which a plan runs while no workload holds it. A helper
-- identifier names one live volume on its node, or in its store.

ALTER TABLE volume ADD COLUMN store text;

DROP INDEX volume_live;
CREATE UNIQUE INDEX volume_live ON volume (node_id, helper_id)
    WHERE deleted_at IS NULL AND store IS NULL;
CREATE UNIQUE INDEX volume_live_shared ON volume (store, helper_id)
    WHERE deleted_at IS NULL AND store IS NOT NULL;

-- A store belongs to the organization of the first node that declared it, and no node of another
-- organization is given it: what a node declares of its mount is its own word, and a store is
-- what lets one node's report reach a volume another node made (Finding 201 of
-- docs/23-walkthroughs.md).
CREATE TABLE shared_store (
    id               text PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    first_node       uuid NOT NULL REFERENCES node (id),
    bound_at         timestamptz NOT NULL
);

GRANT SELECT, INSERT ON shared_store TO korpis_app;
