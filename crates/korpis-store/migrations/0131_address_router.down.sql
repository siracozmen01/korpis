-- SPDX-License-Identifier: AGPL-3.0-or-later

DROP TRIGGER dedicated_address_notify_changed ON dedicated_address;
DROP TRIGGER dedicated_address_notify ON dedicated_address;
CREATE TRIGGER dedicated_address_notify AFTER INSERT OR UPDATE ON dedicated_address
    FOR EACH ROW WHEN (NEW.workload_id IS NOT NULL)
    EXECUTE FUNCTION korpis_notify_workload();

ALTER TABLE dedicated_address
    DROP COLUMN route_held_until,
    DROP COLUMN route_due,
    DROP COLUMN route_attempts,
    DROP COLUMN route_reason,
    DROP COLUMN routed_node_id;

ALTER TABLE address_pool RENAME COLUMN router TO router_id;
ALTER TABLE address_pool ALTER COLUMN router_id TYPE uuid USING NULL;
ALTER TABLE address_pool ADD CONSTRAINT address_pool_router_id_fkey
    FOREIGN KEY (router_id) REFERENCES extension (id);
