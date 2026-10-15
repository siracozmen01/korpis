-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A node's loss events, for the scheduler's churn signal (§2.2 of docs/05-scheduling.md): each
-- time the silent-node sweep marked a node unreachable while it held at least one placement, with
-- how many it held. One event per silence, however many workloads it held. Rows older than the
-- signal remembers are never read and may be deleted.
CREATE TABLE node_loss (
    node_id    uuid NOT NULL REFERENCES node (id),
    at         timestamptz NOT NULL,
    workloads  integer NOT NULL CHECK (workloads > 0),
    PRIMARY KEY (node_id, at)
);
GRANT SELECT, INSERT, DELETE ON node_loss TO korpis_app;
