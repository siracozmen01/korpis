-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Volumes (§3.6 of docs/01-model.md, §7 of docs/06-storage.md): kept from what nodes report,
-- outliving the workloads they were made for, and never removed: a deleted volume is marked.

CREATE TABLE volume (
    id              uuid PRIMARY KEY,
    project_id      uuid NOT NULL REFERENCES project (id),
    -- The workload it was created for, and the one it is attached to while one holds it.
    created_by      uuid NOT NULL REFERENCES workload (id),
    attached_to     uuid REFERENCES workload (id),
    name            text NOT NULL,
    kind            text NOT NULL CHECK (kind IN ('declared', 'stream')),
    node_id         uuid NOT NULL REFERENCES node (id),
    -- The privileged helper's identifier for it on its node.
    helper_id       text NOT NULL,
    storage_class   text NOT NULL,
    size_bytes      bigint NOT NULL CHECK (size_bytes >= 0),
    quota_enforced  boolean NOT NULL,
    created_at      timestamptz NOT NULL,
    detached_at     timestamptz,
    deleted_at      timestamptz
);

-- A helper identifier names one live volume on its node; once that volume is deleted, the name
-- may be ensured again, and that is a new volume.
CREATE UNIQUE INDEX volume_live ON volume (node_id, helper_id) WHERE deleted_at IS NULL;
CREATE INDEX volume_project ON volume (project_id, id);
CREATE INDEX volume_attached ON volume (attached_to) WHERE attached_to IS NOT NULL;

GRANT SELECT, INSERT, UPDATE ON volume TO korpis_app;
