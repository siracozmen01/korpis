-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Event subscriptions (§5 of docs/15-observability.md): a stored position, not a connection. A
-- subscription belongs to its subject; what it is delivered is judged by that subject's
-- `event.read` grants as they are when each event is sent. One an extension's installation
-- created ends with the installation (§4 of docs/16-extensions.md).

CREATE TABLE event_subscription (
    id            uuid PRIMARY KEY,
    subject_key   text NOT NULL,
    -- The store's own form of the filter: kinds, min_severity, projects, workloads.
    filter        jsonb NOT NULL,
    -- The last event position acknowledged; 0 before the first.
    acked         bigint NOT NULL DEFAULT 0 CHECK (acked >= 0),
    created_at    timestamptz NOT NULL,
    extension_id  uuid REFERENCES extension (id)
);
CREATE INDEX event_subscription_subject ON event_subscription (subject_key, id);
CREATE INDEX event_subscription_extension ON event_subscription (extension_id)
    WHERE extension_id IS NOT NULL;

GRANT SELECT, INSERT, UPDATE, DELETE ON event_subscription TO korpis_app;
