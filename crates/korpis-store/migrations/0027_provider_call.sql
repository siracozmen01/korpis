-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Provider calls (§3.3 of docs/16-extensions.md). The control plane has no leader, so the replica
-- that wants a provider called is rarely the one holding a stream to it: the call is queued here,
-- claimed by the replica holding a stream of the newest registered version with a slot free, sent
-- down it, and finished here by the provider's result, by the stream ending, or by the deadline.
-- Queueing and finishing announce on `korpis_provider_call`, which wakes the dispatchers and the
-- callers; the payload is empty, so one transaction announces once however many rows it touched.

CREATE TABLE provider_call (
    id               uuid PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    type             text NOT NULL,
    name             text NOT NULL,
    request          jsonb NOT NULL,
    deadline         timestamptz NOT NULL,
    created_at       timestamptz NOT NULL,
    -- Set when a replica claims it for one of the streams it holds. Not a foreign key: a
    -- registration is deleted when its stream ends, and the call it was sent to stays as history.
    registration_id  uuid,
    version          text,
    sent_at          timestamptz,
    outcome          jsonb,
    finished_at      timestamptz,
    CONSTRAINT provider_call_sent CHECK (
        (registration_id IS NULL) = (sent_at IS NULL)
        AND (registration_id IS NULL) = (version IS NULL)
    ),
    CONSTRAINT provider_call_finished CHECK ((outcome IS NULL) = (finished_at IS NULL))
);
CREATE INDEX provider_call_queued ON provider_call (organization_id, type, name, created_at)
    WHERE registration_id IS NULL AND finished_at IS NULL;
CREATE INDEX provider_call_in_flight ON provider_call (registration_id)
    WHERE registration_id IS NOT NULL AND finished_at IS NULL;
CREATE INDEX provider_call_finished ON provider_call (finished_at) WHERE finished_at IS NOT NULL;

CREATE FUNCTION korpis_provider_call_announce() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('korpis_provider_call', '');
    RETURN NULL;
END;
$$;

CREATE TRIGGER provider_call_queued AFTER INSERT ON provider_call
    FOR EACH ROW EXECUTE FUNCTION korpis_provider_call_announce();
CREATE TRIGGER provider_call_finished AFTER UPDATE OF finished_at ON provider_call
    FOR EACH ROW WHEN (OLD.finished_at IS NULL AND NEW.finished_at IS NOT NULL)
    EXECUTE FUNCTION korpis_provider_call_announce();

GRANT SELECT, INSERT, UPDATE, DELETE ON provider_call TO korpis_app;
