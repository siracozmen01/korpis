-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The nodes a control plane replica shares its network with (§3.1 of docs/07-networking.md):
-- found by the replica binding to each node's address in the mesh, which succeeds only where
-- that address is its own; recorded with an end, renewed while it holds, so a replica that stops
-- leaves nothing standing. An edge group's probe avoids answering through such a node where it
-- can, since that node's answer to the replica would never cross the edge's link back.
CREATE TABLE node_colocated (
    node_id   uuid NOT NULL REFERENCES node (id) ON DELETE CASCADE,
    replica   uuid NOT NULL,
    until     timestamptz NOT NULL,
    PRIMARY KEY (node_id, replica)
);

GRANT SELECT, INSERT, UPDATE, DELETE ON node_colocated TO korpis_app;
