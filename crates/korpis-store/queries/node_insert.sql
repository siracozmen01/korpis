-- A newly enrolled node.
INSERT INTO node (id, name, organization_id, labels, taints, public_key, capacity, allocatable,
                  drivers, storage_classes, reachability, admin, connectivity, agent_version,
                  last_seen, enrolled_at, version)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
