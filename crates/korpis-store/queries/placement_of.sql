-- The authoritative placement of workload $1.
SELECT workload_id, node_id, intent_id, reserved, bound_at, bound_by, reason, explanation,
       authoritative
  FROM placement WHERE workload_id = $1 AND authoritative
