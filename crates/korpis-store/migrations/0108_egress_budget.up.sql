-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The egress budget (§5.1 of docs/05-scheduling.md, "The egress budget"). An entry's budget and
-- what a spent one does are one document, {"bytes_per_month": n, "floor_bits_per_sec": n} or
-- {"bytes_per_month": n, "cut_off": true}, in the column Phase 0 kept for the exhaustion alone and
-- never filled, since the two are set together or not at all.
ALTER TABLE quota_entry RENAME COLUMN egress_exhausted TO egress_budget;

-- What the workloads an entry counts sent in the month, as last reckoned: written every minute,
-- apart from the entry, so reckoning never waits on admission, which locks the entry.
CREATE TABLE quota_egress (
    entry_id    uuid PRIMARY KEY REFERENCES quota_entry (id) ON DELETE CASCADE,
    month       timestamptz NOT NULL,
    bytes       bigint NOT NULL CHECK (bytes >= 0),
    exhausted   boolean NOT NULL,
    reckoned_at timestamptz NOT NULL
);
GRANT SELECT, INSERT, UPDATE, DELETE ON quota_egress TO korpis_app;

-- What each workload counted by a spent entry is held to, the strictest of its spent entries': a
-- floor, or a cut-off. A row is written as an entry is found spent and removed as it no longer
-- is, and either announces the workload, so its node is told at once in its next assignment
-- (§4.3 of docs/02-architecture.md).
CREATE TABLE workload_egress (
    workload_id uuid PRIMARY KEY REFERENCES workload (id) ON DELETE CASCADE,
    month       timestamptz NOT NULL,
    floor_bits_per_sec bigint CHECK (floor_bits_per_sec > 0),
    cut_off     boolean NOT NULL,
    CHECK (cut_off OR floor_bits_per_sec IS NOT NULL)
);
GRANT SELECT, INSERT, UPDATE, DELETE ON workload_egress TO korpis_app;
CREATE TRIGGER workload_egress_notify AFTER INSERT OR UPDATE OR DELETE ON workload_egress
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_workload();
