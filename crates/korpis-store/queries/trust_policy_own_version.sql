-- The version of organization $1's own policy row, set or removed.
SELECT version FROM trust_policy WHERE organization_id = $1
