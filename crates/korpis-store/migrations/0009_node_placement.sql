-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Nodes belong to an organization and keep two states; placements record what they reserve
-- (§3.4 of docs/01-model.md, §2 of docs/05-scheduling.md).
--
-- A node was enrolled under a grant scoped to one organization (§7 of docs/02-architecture.md)
-- and then belonged to none: its ancestry stopped at itself, so no grant but one naming the node
-- could ever cover it, the root grant included. It now belongs to the organization it was
-- enrolled into, which it serves together with every organization below it.
--
-- One `status` column held both what the operator decided and whether the node is heard, so a
-- cordoned node that dropped out and came back returned as `ready`, uncordoned by a network
-- blip. The two are kept apart, and the status a client shows is derived from them.

ALTER TABLE node
    ADD COLUMN organization_id uuid REFERENCES organization (id),
    ADD COLUMN admin text NOT NULL DEFAULT 'active'
        CHECK (admin IN ('active', 'cordoned', 'draining', 'decommissioned')),
    ADD COLUMN connectivity text NOT NULL DEFAULT 'registering'
        CHECK (connectivity IN ('registering', 'connected', 'unreachable', 'unknown')),
    ADD COLUMN version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    ADD CONSTRAINT node_reachability CHECK (reachability IN ('public', 'edge_only'));

-- Nodes enrolled before ownership existed belong to the root organization, the only owner the
-- installation had. After this restart nothing is known of any of them until each is heard.
UPDATE node SET
    organization_id = (SELECT id FROM organization WHERE parent_id IS NULL ORDER BY id LIMIT 1),
    admin = CASE status
        WHEN 'cordoned' THEN 'cordoned'
        WHEN 'draining' THEN 'draining'
        WHEN 'decommissioned' THEN 'decommissioned'
        ELSE 'active' END,
    connectivity = CASE status
        WHEN 'registering' THEN 'registering'
        WHEN 'unreachable' THEN 'unreachable'
        ELSE 'unknown' END;

ALTER TABLE node ALTER COLUMN organization_id SET NOT NULL, DROP COLUMN status;
CREATE INDEX node_organization ON node (organization_id);

-- A placement reserves the reservations of the intent it was bound for; a node's `allocated` is
-- the sum over its placements, authoritative or not, because during a migration both nodes hold
-- the workload's capacity.
ALTER TABLE placement
    ADD COLUMN intent_id uuid NOT NULL REFERENCES intent (id),
    ADD COLUMN reserved jsonb NOT NULL;
CREATE INDEX placement_node ON placement (node_id);

-- The newest attempt that found no node for a workload's target, with the full derivation of
-- why. Kept until a placement is bound, so "why is this not running" has an answer.
CREATE TABLE placement_failure (
    workload_id   uuid PRIMARY KEY REFERENCES workload (id),
    intent_id     uuid NOT NULL REFERENCES intent (id),
    attempted_at  timestamptz NOT NULL,
    explanation   jsonb NOT NULL
);

GRANT SELECT, INSERT, UPDATE, DELETE ON placement_failure TO korpis_app;
