-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The recipes whose content each node holds, as its agent last reported them in ContentStatus,
-- for the scheduler's content locality (§2.2 of docs/05-scheduling.md). Each report replaces the
-- last. `content_reported_at` tells a node that holds nothing from one that never said: content
-- locality is scored only for a node that reported.
CREATE TABLE node_content (
    node_id  uuid NOT NULL REFERENCES node (id),
    digest   text NOT NULL,
    PRIMARY KEY (node_id, digest)
);
GRANT SELECT, INSERT, DELETE ON node_content TO korpis_app;
ALTER TABLE node ADD COLUMN content_reported_at timestamptz;
