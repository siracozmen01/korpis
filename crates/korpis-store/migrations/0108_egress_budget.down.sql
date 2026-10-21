-- SPDX-License-Identifier: AGPL-3.0-or-later

DROP TABLE workload_egress;
DROP TABLE quota_egress;
ALTER TABLE quota_entry RENAME COLUMN egress_budget TO egress_exhausted;
