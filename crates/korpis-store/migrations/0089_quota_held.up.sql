-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Held totals per quota entry (question 7 of §10 of docs/05-scheduling.md). Admission computed
-- what an entry's workloads hold from every one of their intents on each declaration, which an
-- entry on a large organization cannot afford under its lock. Each entry now keeps the totals of
-- what the workloads it counts hold, NULL until they are first reckoned, and each workload keeps
-- what it holds and the entries it was counted by, so that every change to it moves the totals
-- by exactly what it added before. A reconciliation recomputes both from the intents and records
-- any drift it finds rather than trusting the totals.

ALTER TABLE quota_entry ADD COLUMN held jsonb;

CREATE TABLE workload_held (
    workload_id  uuid PRIMARY KEY REFERENCES workload (id),
    held         jsonb NOT NULL,
    counted_by   uuid[] NOT NULL DEFAULT '{}'
);

GRANT SELECT, INSERT, UPDATE ON workload_held TO korpis_app;
