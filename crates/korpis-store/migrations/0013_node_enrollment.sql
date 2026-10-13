-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Enrollment tokens (§3 of docs/18-operations.md, §7 of docs/02-architecture.md). A token is an
-- ordinary Token subject holding a grant of node.enroll with max_uses 1; this row binds to it
-- the organization the node will belong to and the name and labels it will have, so the node
-- an agent enrolls is the one the operator described, whatever the agent says. Once redeemed it
-- names the node, so an agent that lost the answer and asks again with the same key is told
-- the same node rather than refused.

CREATE TABLE node_enrollment (
    token_id         uuid PRIMARY KEY REFERENCES api_token (id),
    grant_id         uuid NOT NULL REFERENCES grants (id),
    organization_id  uuid NOT NULL REFERENCES organization (id),
    node_name        text NOT NULL,
    labels           jsonb NOT NULL DEFAULT '{}',
    created_by       text NOT NULL,
    created_at       timestamptz NOT NULL,
    expires_at       timestamptz NOT NULL,
    node_id          uuid UNIQUE REFERENCES node (id),
    enrolled_at      timestamptz,
    CHECK ((node_id IS NULL) = (enrolled_at IS NULL))
);

GRANT SELECT, INSERT, UPDATE ON node_enrollment TO korpis_app;
