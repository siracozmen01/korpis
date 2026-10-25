-- SPDX-License-Identifier: AGPL-3.0-or-later
-- One node ($1), locked until the transaction ends.
SELECT id, name, organization_id, labels, taints, capacity, allocatable, drivers,
       storage_classes, reachability, admin, connectivity, agent_version, last_seen, enrolled_at,
       version, addresses, first_port, last_port, probed_at, ingress, devices,
       unoffered_devices
  FROM node WHERE id = $1 FOR UPDATE
