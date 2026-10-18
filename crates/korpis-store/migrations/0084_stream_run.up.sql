-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A run's console is a stream of its own (§4 of docs/14-streams.md): a stream is one workload's
-- of one kind, and for a task or scheduled workload one run's, each with its offsets from zero.
-- A persistent workload's stream has no run, and there is one such stream per kind, as before.
-- The run is not a reference: a run pruned by retention is deleted before what its stream holds
-- elsewhere is, and the stream's row stays until that is gone.

ALTER TABLE stream ADD COLUMN run_id uuid;
ALTER TABLE stream DROP CONSTRAINT stream_workload_id_kind_key;
CREATE UNIQUE INDEX stream_of ON stream (workload_id, kind, run_id) NULLS NOT DISTINCT;
