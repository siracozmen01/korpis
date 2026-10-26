-- SPDX-License-Identifier: AGPL-3.0-or-later

DROP TRIGGER stable_port_given_back ON stable_port;
DROP TRIGGER edge_hold_moved ON edge_hold;
ALTER TABLE edge_hold
    DROP COLUMN route_held_until,
    DROP COLUMN route_due,
    DROP COLUMN route_attempts,
    DROP COLUMN route_reason,
    DROP COLUMN routed_node_id;
