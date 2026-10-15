-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The names the ingress routes, one place for the whole installation (§3.2 of
-- docs/07-networking.md): a name routes to one endpoint, so the name is the key. A workload's
-- rows are its approved target's ingress names, written when a plan moves the target and deleted
-- when the workload is retired. The organization is the workload's, kept here so that deleting
-- or delegating a domain can see whose routes it would leave without a holder.

CREATE TABLE route (
    name             text PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    workload_id      uuid NOT NULL REFERENCES workload (id),
    endpoint         text NOT NULL,
    created_at       timestamptz NOT NULL
);

CREATE INDEX route_workload ON route (workload_id);

GRANT SELECT, INSERT, UPDATE, DELETE ON route TO korpis_app;
