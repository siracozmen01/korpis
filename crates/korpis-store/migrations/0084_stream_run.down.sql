-- SPDX-License-Identifier: AGPL-3.0-or-later

DELETE FROM stream_segment WHERE stream_id IN (SELECT id FROM stream WHERE run_id IS NOT NULL);
DELETE FROM stream WHERE run_id IS NOT NULL;
DROP INDEX stream_of;
ALTER TABLE stream ADD CONSTRAINT stream_workload_id_kind_key UNIQUE (workload_id, kind);
ALTER TABLE stream DROP COLUMN run_id;
