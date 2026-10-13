-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Declared sets (§3.3 of docs/01-model.md, §5 and §6 of docs/13-surface-cli.md): a named group of
-- workloads one declarative source owns, in one project. A workload records the set it was
-- created through or adopted into; an intent records the set whose apply declared it. Drift is
-- derived from the two and never stored: a member whose head intent names another set, or none,
-- was changed outside its set.

CREATE TABLE declared_set (
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES project (id),
    name        text NOT NULL,
    mode        text NOT NULL CHECK (mode IN ('advisory', 'strict')),
    version     bigint NOT NULL CHECK (version > 0),
    created_by  text NOT NULL,
    created_at  timestamptz NOT NULL,
    CONSTRAINT declared_set_name_in_project UNIQUE (project_id, name)
);

ALTER TABLE workload ADD COLUMN set_id uuid REFERENCES declared_set (id);
CREATE INDEX workload_set ON workload (set_id) WHERE set_id IS NOT NULL;
ALTER TABLE intent ADD COLUMN managed_by uuid REFERENCES declared_set (id);

GRANT SELECT, INSERT, UPDATE ON declared_set TO korpis_app;
