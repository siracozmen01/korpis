-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Streams and their segment index (§3.6 of docs/01-model.md, §4 of docs/14-streams.md): never
-- the records, only where each run of them is held, kept from what nodes journal.

CREATE TABLE stream (
    id           uuid PRIMARY KEY,
    workload_id  uuid NOT NULL REFERENCES workload (id),
    kind         text NOT NULL CHECK (kind IN ('console', 'log', 'event', 'metric')),
    created_at   timestamptz NOT NULL,
    UNIQUE (workload_id, kind)
);

CREATE TABLE stream_segment (
    stream_id     uuid NOT NULL REFERENCES stream (id),
    first_offset  bigint NOT NULL CHECK (first_offset >= 0),
    end_offset    bigint NOT NULL CHECK (end_offset > first_offset),
    first_at      timestamptz NOT NULL,
    last_at       timestamptz NOT NULL,
    bytes         bigint NOT NULL CHECK (bytes >= 0),
    digest        text NOT NULL,
    -- The node holding it, while one does; cleared when the node prunes it.
    node_id       uuid REFERENCES node (id),
    offloaded     boolean NOT NULL DEFAULT false,
    PRIMARY KEY (stream_id, first_offset)
);

GRANT SELECT, INSERT, UPDATE ON stream TO korpis_app;
GRANT SELECT, INSERT, UPDATE ON stream_segment TO korpis_app;
