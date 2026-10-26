-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Leases on the addresses of edge pools (§3.1 of docs/07-networking.md), fenced as a workload's
-- are (§4.5 of docs/02-architecture.md): each address has the member it was last issued to, its
-- epoch from the same sequence as workloads', the group's term when it was issued, when the
-- control plane last heard the member ask, and when the member said it stopped holding it. A
-- member's health as the control plane last probed it: whether its own address passes the probe
-- through its forwarding path, none until it is probed, when, and why it is not ready; an
-- address's own probe through its holder, likewise. A group's probe is held by one replica at a
-- time for a while, as an address's route is (migration 0131).
CREATE TABLE edge_hold (
    address      inet PRIMARY KEY,
    group_id     uuid NOT NULL REFERENCES edge_group (id) ON DELETE CASCADE,
    node_id      uuid NOT NULL REFERENCES node (id) ON DELETE CASCADE,
    epoch        bigint NOT NULL CHECK (epoch > 0),
    term_ms      integer NOT NULL CHECK (term_ms > 0),
    received_at  timestamptz NOT NULL,
    released_at  timestamptz,
    answering    boolean,
    probed_at    timestamptz
);
CREATE INDEX edge_hold_group ON edge_hold (group_id);

ALTER TABLE edge_group
    ADD COLUMN probe_holder uuid,
    ADD COLUMN probe_held_until timestamptz;

ALTER TABLE edge_member
    ADD COLUMN ready boolean,
    ADD COLUMN probed_at timestamptz,
    ADD COLUMN reason text;

GRANT SELECT, INSERT, UPDATE, DELETE ON edge_hold TO korpis_app;
GRANT UPDATE ON edge_member TO korpis_app;
