-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- What a node advertises for inbound traffic, and when its reachability was last measured (§9 of
-- docs/07-networking.md). Reachability is measured against what is advertised, so a change of
-- either makes it unmeasured again, which is edge_only.

ALTER TABLE node
    ADD COLUMN addresses  text[] NOT NULL DEFAULT '{}',
    ADD COLUMN first_port integer,
    ADD COLUMN last_port  integer,
    ADD COLUMN probed_at  timestamptz,
    ADD CONSTRAINT node_ports CHECK (
        (first_port IS NULL) = (last_port IS NULL)
        AND (first_port IS NULL OR (first_port >= 1024 AND last_port BETWEEN first_port AND 65535))
    );
