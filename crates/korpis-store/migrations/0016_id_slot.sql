-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Host id ranges for user namespaces (§4.5 of docs/04-runtimes.md): each workload placed on a
-- node holds one slot there, a range of 65536 ids, unique on that node while the workload is
-- there. A slot is given back when its workload is retired, since its runtime object is gone
-- and its files carry workload-relative ids. Unique per node rather than per installation, so
-- the 32-bit id space bounds how many workloads one machine holds, never how many a cluster does.

CREATE TABLE node_id_slot (
    node_id      uuid NOT NULL REFERENCES node (id),
    workload_id  uuid NOT NULL REFERENCES workload (id),
    slot         integer NOT NULL CHECK (slot >= 0),
    PRIMARY KEY (node_id, workload_id),
    UNIQUE (node_id, slot)
);

GRANT SELECT, INSERT, DELETE ON node_id_slot TO korpis_app;
