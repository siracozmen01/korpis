-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Each node's workload pool, a /22 of the installation's range given when it enrolled, so no two
-- nodes share one and the mesh can route workloads' addresses between them (§9.1 of
-- docs/07-networking.md). The application chooses the lowest free pool under a lock; the unique
-- key keeps two from being written for one network all the same. A node enrolled before pools
-- were given has none here and goes on with the agent's default.
CREATE TABLE node_pool (
    node_id uuid PRIMARY KEY REFERENCES node (id) ON DELETE CASCADE,
    ipv4    cidr NOT NULL UNIQUE
);

GRANT SELECT, INSERT, DELETE ON node_pool TO korpis_app;
