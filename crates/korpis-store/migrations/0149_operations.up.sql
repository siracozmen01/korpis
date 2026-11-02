-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Operations: one approval over many items, each item a workload or a grant, fixed when the
-- operation is approved. A plan an operation generated names it. At most one operation that has
-- not ended is over a node at a time, since two drains of one node would race for its workloads.

CREATE TABLE operation (
    id                uuid PRIMARY KEY,
    kind              text NOT NULL CHECK (kind IN ('drain', 'evacuate', 'template_rollout')),
    node_id           uuid REFERENCES node (id),
    template_id       uuid REFERENCES grant_template (id),
    template_version  bigint,
    concurrency       integer NOT NULL CHECK (concurrency BETWEEN 1 AND 64),
    max_unavailable   integer NOT NULL CHECK (max_unavailable >= 1),
    on_failure        text NOT NULL CHECK (on_failure IN ('pause', 'continue', 'abort')),
    status            text NOT NULL CHECK (status IN (
                          'proposed', 'approved', 'running', 'paused', 'completed', 'failed',
                          'aborted')),
    reason            text NOT NULL DEFAULT '',
    proposed_by       text NOT NULL,
    proposed_under    uuid NOT NULL REFERENCES grants (id),
    proposed_at       timestamptz NOT NULL,
    approved_by       text,
    approved_under    uuid REFERENCES grants (id),
    approved_mfa      boolean,
    approved_at       timestamptz,
    start_after       timestamptz,
    ended_at          timestamptz,
    version           bigint NOT NULL DEFAULT 0,
    CHECK ((kind IN ('drain', 'evacuate')) = (node_id IS NOT NULL)),
    CHECK ((kind = 'template_rollout') = (template_id IS NOT NULL)),
    CHECK ((template_id IS NULL) = (template_version IS NULL)),
    CHECK ((approved_by IS NULL) = (approved_at IS NULL)),
    CHECK ((approved_by IS NULL) = (approved_under IS NULL)),
    CHECK ((approved_by IS NULL) = (approved_mfa IS NULL)),
    CHECK ((status IN ('completed', 'failed', 'aborted')) = (ended_at IS NOT NULL))
);
CREATE UNIQUE INDEX operation_one_per_node ON operation (node_id)
    WHERE status NOT IN ('completed', 'failed', 'aborted');
CREATE INDEX operation_live ON operation (proposed_at)
    WHERE status IN ('approved', 'running', 'paused');

CREATE TABLE operation_item (
    operation_id  uuid NOT NULL REFERENCES operation (id),
    subject       uuid NOT NULL,
    position      integer NOT NULL CHECK (position >= 0),
    state         text NOT NULL CHECK (state IN (
                      'waiting', 'applying', 'settling', 'done', 'failed', 'skipped')),
    plan_id       uuid REFERENCES plan (id),
    issued        uuid REFERENCES grants (id),
    detail        jsonb NOT NULL DEFAULT '{}',
    reason        text NOT NULL DEFAULT '',
    begun_at      timestamptz,
    ended_at      timestamptz,
    PRIMARY KEY (operation_id, subject),
    UNIQUE (operation_id, position),
    CHECK ((state IN ('done', 'failed', 'skipped')) = (ended_at IS NOT NULL))
);

ALTER TABLE plan ADD CONSTRAINT plan_operation_fkey
    FOREIGN KEY (operation_id) REFERENCES operation (id);
CREATE INDEX plan_operation ON plan (operation_id) WHERE operation_id IS NOT NULL;
