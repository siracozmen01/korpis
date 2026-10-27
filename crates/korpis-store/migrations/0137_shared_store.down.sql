-- SPDX-License-Identifier: AGPL-3.0-or-later

DROP TABLE shared_store;
DROP INDEX volume_live_shared;
DROP INDEX volume_live;
CREATE UNIQUE INDEX volume_live ON volume (node_id, helper_id) WHERE deleted_at IS NULL;
ALTER TABLE volume DROP COLUMN store;
