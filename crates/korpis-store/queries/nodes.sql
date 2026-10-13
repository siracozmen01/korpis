-- The nodes organization $1 owns, in identifier order after $2, at most $3.
SELECT id, name, organization_id, labels, taints, capacity, allocatable, drivers,
       storage_classes, reachability, admin, connectivity, agent_version, last_seen, enrolled_at,
       version, addresses, first_port, last_port, probed_at
  FROM node WHERE organization_id = $1 AND ($2::uuid IS NULL OR id > $2)
 ORDER BY id LIMIT $3
