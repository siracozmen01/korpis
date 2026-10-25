-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A node's side of the mesh as its agent last reported it, in full with every report (§9.1 of
-- docs/07-networking.md): its interface's public key and port, its workload pools, and when each
-- peer last completed a handshake with it, from which the control plane judges which pairs reach
-- each other. NULL for a node whose helper makes no workload networks.
ALTER TABLE node ADD COLUMN overlay jsonb;
