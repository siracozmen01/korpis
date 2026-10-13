-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Provider circuit breakers (§5 of docs/16-extensions.md). One per provider as organizations see
-- it, by organization, type, and name, shared by every replica, because any replica may call any
-- provider. A breaker is judged from the calls the provider last finished, which the queue of
-- migration 0027 already holds, so the evidence is never a second copy; the row holds only the
-- decision, and is locked by whoever changes it, so two replicas cannot both trip it.

CREATE TABLE provider_breaker (
    organization_id  uuid NOT NULL REFERENCES organization (id),
    type             text NOT NULL,
    name             text NOT NULL,
    state            text NOT NULL CHECK (state IN ('closed', 'open', 'half_open')),
    -- Open: when it probes. Half open: when its probe's deadline passes.
    until            timestamptz,
    backoff_ms       bigint CHECK (backoff_ms > 0),
    probe            uuid,
    changed_at       timestamptz NOT NULL,
    PRIMARY KEY (organization_id, type, name),
    CONSTRAINT provider_breaker_shape CHECK (
        (state = 'closed' AND until IS NULL AND backoff_ms IS NULL AND probe IS NULL)
        OR (state = 'open' AND until IS NOT NULL AND backoff_ms IS NOT NULL AND probe IS NULL)
        OR (state = 'half_open' AND until IS NOT NULL AND backoff_ms IS NOT NULL
            AND probe IS NOT NULL)
    )
);

-- The calls a provider last finished, newest first, for judging it.
CREATE INDEX provider_call_recent ON provider_call (organization_id, type, name, finished_at DESC)
    WHERE finished_at IS NOT NULL;

GRANT SELECT, INSERT, UPDATE, DELETE ON provider_breaker TO korpis_app;
