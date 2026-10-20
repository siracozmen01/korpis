-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Acting as an external identity (§5.2 of docs/08-identity.md). A presentation, the provider's
-- identifier for what one person presented once, is remembered for fifteen minutes so it is
-- accepted once: a captured chat interaction relayed again is refused here, whatever the
-- provider says of it. A ticket is what the relaying client is handed: one action, on one
-- object, once, within a minute, by that client alone; it is kept as its secret's digest.

CREATE TABLE identity_presentation (
    organization_id  uuid NOT NULL REFERENCES organization (id),
    provider         text NOT NULL,
    presentation     text NOT NULL,
    accepted_at      timestamptz NOT NULL,
    PRIMARY KEY (organization_id, provider, presentation)
);
CREATE INDEX identity_presentation_accepted ON identity_presentation (accepted_at);

CREATE TABLE identity_ticket (
    id               uuid PRIMARY KEY,
    digest           bytea NOT NULL UNIQUE,
    -- The principal key of the client that relayed it, the only one that may use it.
    relay            text NOT NULL,
    -- The principal key of the asserted person.
    subject          text NOT NULL,
    sets             jsonb NOT NULL,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    provider         text NOT NULL,
    presentation     text NOT NULL,
    action           text NOT NULL,
    object_kind      text NOT NULL,
    object_id        uuid NOT NULL,
    expires_at       timestamptz NOT NULL,
    used_at          timestamptz
);
CREATE INDEX identity_ticket_expires ON identity_ticket (expires_at);

GRANT SELECT, INSERT, DELETE ON identity_presentation TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON identity_ticket TO korpis_app;
