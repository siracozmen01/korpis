-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Where an offloaded segment is (§4 of docs/14-streams.md): the repository holding it, rather
-- than only that one does, so it can be read back once its node is gone. Offload was built a day
-- before this and no index row is marked offloaded, so the flag is replaced rather than kept.
--
-- A node's word that it offloaded a segment there is believed only if the control plane gave
-- that node write access to the workload's streams there, so access records the node given it;
-- NULL is the control plane itself.

ALTER TABLE stream_segment ADD COLUMN repository_id uuid REFERENCES repository (id);
ALTER TABLE stream_segment DROP COLUMN offloaded;


ALTER TABLE backup_access ADD COLUMN node_id uuid REFERENCES node (id);
