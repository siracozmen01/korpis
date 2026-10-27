-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A volume's replicas (§2.2 of docs/06-storage.md): each a node the control plane chose to keep a
-- copy, with the snapshot it last received whole and when, the transfer under way to it if any,
-- and when it was marked stale, ten intervals after it last received one, which releases the
-- volume's node from holding the snapshot it was to be sent from.

CREATE TABLE volume_replica (
    volume_id      uuid NOT NULL REFERENCES volume (id),
    node_id        uuid NOT NULL REFERENCES node (id),
    chosen_at      timestamptz NOT NULL,
    -- The snapshot it holds whole, the base of its next increment; NULL until the first arrives.
    snapshot       uuid,
    received_at    timestamptz,
    -- The snapshot being sent to it, and since when; NULL while none is.
    sending        uuid,
    sending_since  timestamptz,
    stale_at       timestamptz,
    PRIMARY KEY (volume_id, node_id),
    CONSTRAINT volume_replica_received CHECK ((snapshot IS NULL) = (received_at IS NULL)),
    CONSTRAINT volume_replica_sending CHECK ((sending IS NULL) = (sending_since IS NULL))
);

CREATE INDEX volume_replica_node ON volume_replica (node_id);

GRANT SELECT, INSERT, UPDATE, DELETE ON volume_replica TO korpis_app;
