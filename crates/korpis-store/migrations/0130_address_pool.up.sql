-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Address pools and the dedicated addresses given from them (§3.7 and §4 of
-- docs/07-networking.md). A pool belongs to an organization and serves the workloads of its
-- line: a `node` pool's addresses are routed to one node, a `cluster` pool's moved to whichever
-- node holds one by an address router, an extension installation. Pools never overlap, so an
-- address is in one pool at most, and an address is held by one workload at most: the key is the
-- address. A released address keeps its row, with when it was released, until its pool's
-- quarantine has passed; the application chooses the lowest address of a pool with no row, or
-- whose rest has ended, under the placement lock.
CREATE TABLE address_pool (
    id               uuid PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    family           text NOT NULL CHECK (family IN ('ipv4', 'ipv6')),
    cidr             cidr NOT NULL,
    scope            text NOT NULL CHECK (scope IN ('node', 'edge', 'cluster')),
    node_id          uuid REFERENCES node (id),
    router_id        uuid REFERENCES extension (id),
    routing          text NOT NULL CHECK (routing IN ('static', 'bgp', 'provider_api')),
    interface        text NOT NULL DEFAULT '',
    quarantine_secs  bigint NOT NULL CHECK (quarantine_secs >= 0),
    reserved         inet[] NOT NULL DEFAULT '{}',
    created_at       timestamptz NOT NULL,
    CHECK ((scope = 'node') = (node_id IS NOT NULL)),
    CHECK ((scope = 'cluster') = (router_id IS NOT NULL)),
    CHECK ((scope = 'node') = (routing = 'static')),
    CHECK (family(cidr) = CASE family WHEN 'ipv4' THEN 4 ELSE 6 END)
);
-- No two pools give one address.
ALTER TABLE address_pool ADD CONSTRAINT address_pool_disjoint
    EXCLUDE USING gist (cidr inet_ops WITH &&);

CREATE TABLE dedicated_address (
    address      inet PRIMARY KEY,
    pool_id      uuid NOT NULL REFERENCES address_pool (id) ON DELETE CASCADE,
    workload_id  uuid UNIQUE REFERENCES workload (id),
    held_at      timestamptz NOT NULL,
    released_at  timestamptz,
    CHECK ((workload_id IS NULL) = (released_at IS NOT NULL))
);

GRANT SELECT, INSERT, DELETE ON address_pool TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON dedicated_address TO korpis_app;
-- An address given changes its holder's assignment; one taken back is taken from a workload
-- whose retirement has already told its node.
CREATE TRIGGER dedicated_address_notify AFTER INSERT OR UPDATE ON dedicated_address
    FOR EACH ROW WHEN (NEW.workload_id IS NOT NULL)
    EXECUTE FUNCTION korpis_notify_workload();
