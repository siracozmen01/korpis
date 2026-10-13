-- What node $1's agent reported at $8, and that it is connected and was heard then. The agent's
-- release and protocol ($6, $7) and its storage classes ($5) are kept as they were when a
-- heartbeat does not say. Facts are
-- not operator decisions, so the node's version, which guards those, does not move.
UPDATE node SET capacity = $2, allocatable = $3, drivers = $4,
                storage_classes = COALESCE($5, storage_classes),
                agent_version = COALESCE($6, agent_version), protocol = COALESCE($7, protocol),
                connectivity = 'connected', last_seen = $8
 WHERE id = $1 AND admin <> 'decommissioned'
