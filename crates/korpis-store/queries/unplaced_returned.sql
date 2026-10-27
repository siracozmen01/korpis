-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Forgets each failover that found no node, for the intent a workload is placed for, once its
-- node is heard again (§2.4 of docs/05-scheduling.md): the attempt was made after the placement
-- was bound, which no other attempt for the same intent is, since binding forgets them.
DELETE FROM placement_failure f
 USING placement p, node n
 WHERE p.workload_id = f.workload_id AND p.authoritative AND p.intent_id = f.intent_id
   AND f.since > p.bound_at
   AND n.id = p.node_id AND n.connectivity <> 'unreachable'
