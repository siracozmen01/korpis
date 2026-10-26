-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The routes of routed edge pools' addresses (§3.1 of docs/07-networking.md), kept as a cluster
-- pool's are (migration 0131): each address's lease keeps the node its pool's router last said it
-- routes the address to, why its last answer was not that, how many calls have failed in a row,
-- when it is asked again, and until when a control plane holds it to ask. A lease issued to
-- another member, one released, and a port given back wake the reconciler, as an endpoint moved
-- does (migration 0063); a new member is asked for at once, whatever the last one's backoff.
ALTER TABLE edge_hold
    ADD COLUMN routed_node_id uuid REFERENCES node (id) ON DELETE SET NULL,
    ADD COLUMN route_reason text,
    ADD COLUMN route_attempts integer NOT NULL DEFAULT 0 CHECK (route_attempts >= 0),
    ADD COLUMN route_due timestamptz,
    ADD COLUMN route_held_until timestamptz;

CREATE TRIGGER edge_hold_moved AFTER INSERT OR UPDATE OF node_id, released_at ON edge_hold
    FOR EACH ROW EXECUTE FUNCTION korpis_endpoint_announce();

CREATE TRIGGER stable_port_given_back AFTER UPDATE OF workload_id ON stable_port
    FOR EACH ROW WHEN (OLD.workload_id IS NOT NULL AND NEW.workload_id IS NULL)
    EXECUTE FUNCTION korpis_endpoint_announce();
