-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Edge groups and the stable addresses and ports of `stable` endpoints (§3.1 of
-- docs/07-networking.md, §3.6 of docs/01-model.md). A group belongs to an organization, and its
-- members are nodes of that organization; an address pool of scope `edge` names the group whose
-- members hold its addresses, statically on a link they share or moved by the router it names.
-- A `stable` endpoint holds an address and port of such a pool, one row for each family it is
-- given, kept with its workload whatever node the workload runs on; released, the row rests until
-- its pool's quarantine has passed. A port is held per transport, so a game's TCP port and its
-- query's UDP port may share a number, as Minecraft's do: the key is the address, the port, and
-- the transport.
CREATE TABLE edge_group (
    id                 uuid PRIMARY KEY,
    organization_id    uuid NOT NULL REFERENCES organization (id),
    name               text NOT NULL,
    first_port         integer NOT NULL CHECK (first_port BETWEEN 1 AND 65535),
    last_port          integer NOT NULL CHECK (last_port BETWEEN first_port AND 65535),
    probe_port         integer NOT NULL CHECK (probe_port BETWEEN 1 AND 65535),
    probe_interval_ms  integer NOT NULL CHECK (probe_interval_ms BETWEEN 100 AND 60000),
    probe_misses       integer NOT NULL CHECK (probe_misses BETWEEN 1 AND 100),
    term_ms            integer NOT NULL CHECK (term_ms BETWEEN 3000 AND 600000),
    created_at         timestamptz NOT NULL,
    UNIQUE (organization_id, name),
    CHECK (probe_port NOT BETWEEN first_port AND last_port)
);

CREATE TABLE edge_member (
    group_id  uuid NOT NULL REFERENCES edge_group (id) ON DELETE CASCADE,
    node_id   uuid NOT NULL REFERENCES node (id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, node_id)
);
CREATE INDEX edge_member_node ON edge_member (node_id);

-- An edge pool names its group, and is static exactly where it names no router.
ALTER TABLE address_pool ADD COLUMN group_id uuid REFERENCES edge_group (id);
ALTER TABLE address_pool DROP CONSTRAINT address_pool_check1;
ALTER TABLE address_pool DROP CONSTRAINT address_pool_check2;
ALTER TABLE address_pool ADD CONSTRAINT address_pool_group
    CHECK ((scope = 'edge') = (group_id IS NOT NULL));
ALTER TABLE address_pool ADD CONSTRAINT address_pool_routed CHECK (
    CASE scope
        WHEN 'node' THEN router IS NULL AND routing = 'static'
        WHEN 'cluster' THEN router IS NOT NULL AND routing <> 'static'
        ELSE (router IS NULL) = (routing = 'static')
    END);

CREATE TABLE stable_port (
    address      inet NOT NULL,
    port         integer NOT NULL CHECK (port BETWEEN 1 AND 65535),
    transport    text NOT NULL CHECK (transport IN ('tcp', 'udp')),
    pool_id      uuid NOT NULL REFERENCES address_pool (id) ON DELETE CASCADE,
    workload_id  uuid REFERENCES workload (id),
    endpoint     text,
    held_at      timestamptz NOT NULL,
    released_at  timestamptz,
    PRIMARY KEY (address, port, transport),
    CHECK ((workload_id IS NULL) = (released_at IS NOT NULL)),
    CHECK ((workload_id IS NULL) = (endpoint IS NULL))
);
-- An endpoint holds one address and port of each family at most.
CREATE UNIQUE INDEX stable_port_endpoint ON stable_port (workload_id, endpoint, family(address))
    WHERE workload_id IS NOT NULL;
CREATE INDEX stable_port_pool ON stable_port (pool_id);

GRANT SELECT, INSERT, UPDATE, DELETE ON edge_group TO korpis_app;
GRANT SELECT, INSERT, DELETE ON edge_member TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON stable_port TO korpis_app;
-- A port given or taken back changes what its holder's status shows.
CREATE TRIGGER stable_port_notify AFTER INSERT OR UPDATE ON stable_port
    FOR EACH ROW WHEN (NEW.workload_id IS NOT NULL)
    EXECUTE FUNCTION korpis_notify_workload();
