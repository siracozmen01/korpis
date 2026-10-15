-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The nodes any organization of $1 owns, decommissioned ones excepted, in identifier order.
SELECT id, name, organization_id, labels, taints, capacity, allocatable, drivers,
       storage_classes, reachability, admin, connectivity, agent_version, last_seen, enrolled_at,
       version, addresses, first_port, last_port, probed_at, ingress
  FROM node WHERE organization_id = ANY($1) AND admin <> 'decommissioned'
 ORDER BY id
