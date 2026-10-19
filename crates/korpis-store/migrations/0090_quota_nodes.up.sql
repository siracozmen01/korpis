-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A quota entry that counts where workloads run (question 6 of §10 of docs/05-scheduling.md):
-- "4 GB in the EU" counts what runs on the nodes labelled for the EU, which is known only once
-- a workload is placed. Such an entry names those nodes by a selector over their labels; it is
-- checked when a workload is bound, and its totals move only under the placement lock, as every
-- binding and unbinding is made. What each placement was counted as holding, and by which of
-- these entries, is kept beside it, so taking it back out is exact.

ALTER TABLE quota_entry ADD COLUMN nodes jsonb;

CREATE TABLE placement_held (
    workload_id  uuid PRIMARY KEY REFERENCES workload (id),
    node_id      uuid NOT NULL REFERENCES node (id),
    held         jsonb NOT NULL,
    counted_by   uuid[] NOT NULL DEFAULT '{}'
);

GRANT SELECT, INSERT, UPDATE, DELETE ON placement_held TO korpis_app;
