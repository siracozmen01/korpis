-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Endpoints as placed (§3.6 of docs/01-model.md, §3 of docs/07-networking.md): one row for each
-- endpoint a placed workload's target declares, written when the control plane computes the
-- node's assignment, with the exposure the declared one resolved to and, for node_port and
-- node_bound, the port of the node it holds. A port is unique on its node and stays the
-- endpoint's while the workload is placed there and declares it; it is given back when the
-- workload is retired, moves, or stops declaring the endpoint.

CREATE TABLE endpoint (
    id              uuid PRIMARY KEY,
    workload_id     uuid NOT NULL REFERENCES workload (id),
    name            text NOT NULL,
    protocol        text NOT NULL CHECK (protocol IN ('tcp', 'udp', 'http', 'https', 'quic')),
    container_port  integer NOT NULL CHECK (container_port BETWEEN 1 AND 65535),
    exposure        text NOT NULL CHECK (exposure IN ('node_port', 'node_bound', 'stable',
                                                      'ingress', 'dedicated_ip',
                                                      'overlay_only', 'none')),
    node_id         uuid NOT NULL REFERENCES node (id),
    port            integer CHECK (port BETWEEN 1024 AND 65535),
    created_at      timestamptz NOT NULL,
    UNIQUE (workload_id, name),
    UNIQUE (node_id, port)
);

CREATE INDEX endpoint_node ON endpoint (node_id);

GRANT SELECT, INSERT, UPDATE, DELETE ON endpoint TO korpis_app;
