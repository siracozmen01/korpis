-- One node ($1).
SELECT id, name, organization_id, labels, taints, capacity, allocatable, drivers,
       storage_classes, reachability, admin, connectivity, agent_version, last_seen, enrolled_at,
       version
  FROM node WHERE id = $1
