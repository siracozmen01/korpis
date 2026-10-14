-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- How long offloaded segments are kept (§4 of docs/14-streams.md): from when the control plane
-- heard each was offloaded, on its own clock, for what the workload's stream declares, raised to
-- the operator's disclosed floor (§8 of docs/17-security.md). The declared retention is kept on
-- the stream, refreshed from the intent its workload runs at each segment the node reports, so one
-- query finds every segment past it; a retired workload keeps the last it declared.
--
-- A stream is deleted by the step of an approved plan that deletes its volume; from then its
-- offloaded segments are kept no longer than the operator's floor.

ALTER TABLE stream ADD COLUMN offload_retention_s bigint NOT NULL DEFAULT 2592000
    CHECK (offload_retention_s > 0);
ALTER TABLE stream ADD COLUMN deleted_at timestamptz;
ALTER TABLE stream_segment ADD COLUMN offloaded_at timestamptz;
ALTER TABLE stream_segment ADD CONSTRAINT stream_segment_offloaded
    CHECK ((repository_id IS NULL) = (offloaded_at IS NULL));
CREATE INDEX stream_segment_offloaded_at ON stream_segment (offloaded_at)
    WHERE repository_id IS NOT NULL;
