-- SPDX-License-Identifier: AGPL-3.0-or-later

REVOKE UPDATE ON edge_member FROM korpis_app;
ALTER TABLE edge_member DROP COLUMN reason, DROP COLUMN probed_at, DROP COLUMN ready;
DROP TABLE edge_hold;
ALTER TABLE edge_group DROP COLUMN probe_held_until, DROP COLUMN probe_holder;
