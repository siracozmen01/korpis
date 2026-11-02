-- SPDX-License-Identifier: AGPL-3.0-or-later

DROP INDEX plan_operation;
ALTER TABLE plan DROP CONSTRAINT plan_operation_fkey;
UPDATE plan SET operation_id = NULL WHERE operation_id IS NOT NULL;
DROP TABLE operation_item;
DROP TABLE operation;
