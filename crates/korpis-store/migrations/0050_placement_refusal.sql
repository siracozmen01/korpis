-- A node whose driver refused a workload's target (§2.1 of docs/05-scheduling.md): asked on the
-- shortlist before a bind, or after one. The scheduler leaves such a node out when it places that
-- target, and the explanation says why. A new target is asked again.
CREATE TABLE placement_refusal (
    workload_id  uuid NOT NULL REFERENCES workload (id),
    intent_id    uuid NOT NULL REFERENCES intent (id),
    node_id      uuid NOT NULL REFERENCES node (id),
    reason       text NOT NULL,
    refused_at   timestamptz NOT NULL,
    PRIMARY KEY (workload_id, intent_id, node_id)
);
GRANT SELECT, INSERT, UPDATE ON placement_refusal TO korpis_app;
