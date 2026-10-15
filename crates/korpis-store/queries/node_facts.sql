-- SPDX-License-Identifier: AGPL-3.0-or-later
-- What node $1's agent reported at $8, and that it is connected and was heard then. The agent's
-- release and protocol ($6, $7), its storage classes ($5), and what it advertises ($9 to $12)
-- are kept as they were when a report does not say. A change of what it advertises makes its
-- reachability unmeasured again (§9 of docs/07-networking.md). Facts are not operator decisions,
-- so the node's version, which guards those, does not move.
UPDATE node SET capacity = $2, allocatable = $3, drivers = $4,
                storage_classes = COALESCE($5, storage_classes),
                agent_version = COALESCE($6, agent_version), protocol = COALESCE($7, protocol),
                connectivity = 'connected', last_seen = $8,
                reachability = CASE WHEN $9::text[] IS NOT NULL
                                     AND ($9, $10::integer, $11::integer)
                                         IS DISTINCT FROM (addresses, first_port, last_port)
                                    THEN 'edge_only' ELSE reachability END,
                probed_at = CASE WHEN $9::text[] IS NOT NULL
                                  AND ($9, $10::integer, $11::integer)
                                      IS DISTINCT FROM (addresses, first_port, last_port)
                                 THEN NULL ELSE probed_at END,
                addresses = COALESCE($9, addresses),
                first_port = CASE WHEN $9::text[] IS NULL THEN first_port ELSE $10 END,
                last_port = CASE WHEN $9::text[] IS NULL THEN last_port ELSE $11 END,
                ingress = CASE WHEN $9::text[] IS NULL THEN ingress ELSE $12::boolean END
 WHERE id = $1 AND admin <> 'decommissioned'
