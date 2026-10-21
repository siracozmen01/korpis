-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Each quota entry with an egress budget, and each workload it selected in a month, by owner and
-- labels (§5.1 of docs/05-scheduling.md, "The egress budget"). An entry counts a workload it has
-- selected once in a month for the rest of that month, so a holder who relabels a held workload
-- out of a spent entry's selector neither lifts its hold nor takes what it sent out of the entry's
-- sum (Finding 126 of docs/23-walkthroughs.md). A month's rows are forgotten once it has ended.
CREATE TABLE quota_egress_selected (
    entry_id    uuid NOT NULL REFERENCES quota_entry (id) ON DELETE CASCADE,
    workload_id uuid NOT NULL REFERENCES workload (id) ON DELETE CASCADE,
    month       timestamptz NOT NULL,
    PRIMARY KEY (month, entry_id, workload_id)
);
GRANT SELECT, INSERT, UPDATE, DELETE ON quota_egress_selected TO korpis_app;
