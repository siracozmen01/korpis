-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Metering's gaps and periods (§2 of docs/15-observability.md). An interval of a workload that no
-- record covers two intervals after it ended is written as a gap, one record an interval, and a
-- measurement that arrives later supersedes every gap it covers, so a record supersedes a list.
-- A day closes once its lateness window passes, and a record of a closed day is an adjustment.

ALTER TABLE metering_record
    ALTER COLUMN supersedes TYPE uuid[]
        USING (CASE WHEN supersedes IS NULL THEN '{}'::uuid[] ELSE ARRAY[supersedes] END),
    ALTER COLUMN supersedes SET DEFAULT '{}',
    ALTER COLUMN supersedes SET NOT NULL;
CREATE INDEX metering_record_supersedes ON metering_record USING gin (supersedes);
CREATE INDEX metering_record_gaps ON metering_record (workload_id, node_id, interval_start)
    WHERE kind = 'gap';

-- How far gaps have been written past a workload's last sample on a node.
ALTER TABLE metering_reading ADD COLUMN gapped_through timestamptz;

-- The days closed, and when: a day not listed is open while it runs and closing until its
-- lateness window passes.
CREATE TABLE metering_period (
    day        date PRIMARY KEY,
    closed_at  timestamptz NOT NULL
);
GRANT SELECT, INSERT ON metering_period TO korpis_app;
