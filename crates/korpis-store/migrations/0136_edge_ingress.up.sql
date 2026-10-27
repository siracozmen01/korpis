-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- `ingress` through an edge group (§3.1 of docs/07-networking.md). A group's ingress address is
-- one address of its pools of each family, ports 80 and 443 of which, over TCP and UDP, no
-- `stable` endpoint is given; it is chosen at the first placement whose ingress the group serves
-- and kept for as long as its pool is, so the names that resolve to it do not move. A workload
-- whose `ingress` endpoints a group serves is recorded with the group, from the placement that
-- resolves them so until one that does not or its retirement. Each node says which challenges
-- of the names its groups serve its proxy holds, which the issuer waits on before the directory
-- is told to look, as it waits on a node's acknowledged set for the names its workloads route.
CREATE TABLE edge_ingress (
    address   inet PRIMARY KEY,
    group_id  uuid NOT NULL REFERENCES edge_group (id) ON DELETE CASCADE,
    pool_id   uuid NOT NULL REFERENCES address_pool (id) ON DELETE CASCADE,
    held_at   timestamptz NOT NULL
);
CREATE UNIQUE INDEX edge_ingress_family ON edge_ingress (group_id, family(address));
CREATE INDEX edge_ingress_pool ON edge_ingress (pool_id);

CREATE TABLE ingress_served (
    workload_id  uuid PRIMARY KEY REFERENCES workload (id) ON DELETE CASCADE,
    group_id     uuid NOT NULL REFERENCES edge_group (id) ON DELETE CASCADE,
    held_at      timestamptz NOT NULL
);
CREATE INDEX ingress_served_group ON ingress_served (group_id);

CREATE TABLE edge_challenges_held (
    node_id   uuid PRIMARY KEY REFERENCES node (id) ON DELETE CASCADE,
    tokens    text[] NOT NULL,
    digests   bytea[] NOT NULL,
    held_at   timestamptz NOT NULL
);

GRANT SELECT, INSERT, UPDATE, DELETE ON edge_ingress TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON ingress_served TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON edge_challenges_held TO korpis_app;

-- A workload's ingress served or no longer served changes what its status shows, and where its
-- names resolve; an ingress address chosen changes where they resolve too.
CREATE TRIGGER ingress_served_notify AFTER INSERT OR UPDATE OR DELETE ON ingress_served
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_workload();
CREATE TRIGGER ingress_served_announce AFTER INSERT OR UPDATE OR DELETE ON ingress_served
    FOR EACH ROW EXECUTE FUNCTION korpis_endpoint_announce();
CREATE TRIGGER edge_ingress_announce AFTER INSERT OR DELETE ON edge_ingress
    FOR EACH ROW EXECUTE FUNCTION korpis_endpoint_announce();
