-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The workloads an organization's suspension stopped (§2.1 of docs/08-identity.md), each with
-- the power version the suspension declared, so that resuming starts again exactly those whose
-- power no one has changed since: what the tenant stopped itself before, and what anyone started
-- or stopped after, is left as it is. Rows go when the organization is resumed.

CREATE TABLE suspension_stop (
    organization_id  uuid NOT NULL REFERENCES organization (id),
    workload_id      uuid NOT NULL REFERENCES workload (id),
    power_version    bigint NOT NULL CHECK (power_version > 0),
    PRIMARY KEY (organization_id, workload_id)
);

GRANT SELECT, INSERT, DELETE ON suspension_stop TO korpis_app;
