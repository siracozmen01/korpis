-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The router's contract (§3.7 of docs/07-networking.md). A cluster pool names its router as a
-- backup repository names its target, by the name of an `address_router` provider its
-- organization sees, and no longer by an extension, which no provider call is addressed by
-- (corrected in §3.6 of docs/01-model.md); no cluster pool could be made before, so none is
-- changed. Each address keeps the node the router last said it routes the address to, which is
-- the node whose assignment carries it, and the state of asking: why the last call failed, how
-- many have failed since one answered, when to ask next, and until when a control plane holds it
-- while it asks.
ALTER TABLE address_pool DROP CONSTRAINT address_pool_router_id_fkey;
ALTER TABLE address_pool ALTER COLUMN router_id TYPE text USING NULL;
ALTER TABLE address_pool RENAME COLUMN router_id TO router;

ALTER TABLE dedicated_address
    ADD COLUMN routed_node_id uuid REFERENCES node (id) ON DELETE SET NULL,
    ADD COLUMN route_reason text,
    ADD COLUMN route_attempts integer NOT NULL DEFAULT 0 CHECK (route_attempts >= 0),
    ADD COLUMN route_due timestamptz,
    ADD COLUMN route_held_until timestamptz;

-- Its holder's nodes hear of an address given, and of one routed to another node, which changes
-- what the node's assignment carries; not of a control plane taking it to ask its router.
DROP TRIGGER dedicated_address_notify ON dedicated_address;
CREATE TRIGGER dedicated_address_notify AFTER INSERT ON dedicated_address
    FOR EACH ROW WHEN (NEW.workload_id IS NOT NULL)
    EXECUTE FUNCTION korpis_notify_workload();
CREATE TRIGGER dedicated_address_notify_changed AFTER UPDATE ON dedicated_address
    FOR EACH ROW WHEN (NEW.workload_id IS NOT NULL
        AND (NEW.workload_id IS DISTINCT FROM OLD.workload_id
             OR NEW.routed_node_id IS DISTINCT FROM OLD.routed_node_id))
    EXECUTE FUNCTION korpis_notify_workload();
