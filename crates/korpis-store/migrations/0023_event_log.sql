-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The event log (§5 of docs/15-observability.md). Every effect is an event of its own action,
-- written by a trigger in the transaction that appends the effect, so no path can record a fact
-- and skip its event. A key's events are numbered by the key's counter row, which two
-- transactions about one key take in turn, so a key's numbers follow commit order. An event's
-- public identifier is its position, which a sequencer assigns in batches to events whose
-- transactions have all ended (below the snapshot horizon), in insertion order; that order agrees
-- with every key's numbering, so delivering by position never hands out a key's events out of
-- order and never passes over one that commits late.

-- The organization that owns an object, for the kinds that are rows here.
CREATE FUNCTION korpis_object_organization(kind text, object uuid) RETURNS uuid
LANGUAGE sql STABLE AS $$
    SELECT CASE kind
        WHEN 'organization' THEN object
        WHEN 'project' THEN (SELECT organization_id FROM project WHERE id = object)
        WHEN 'workload' THEN (SELECT organization_id FROM workload WHERE id = object)
        WHEN 'node' THEN (SELECT organization_id FROM node WHERE id = object)
    END
$$;

CREATE TABLE event_key (
    key  text PRIMARY KEY,
    seq  bigint NOT NULL CHECK (seq > 0)
);

CREATE TABLE event (
    -- Insertion order, never shown: the sequencer's tie-breaker.
    id               bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The public identifier, assigned once every earlier transaction has ended.
    position         bigint UNIQUE,
    xid              xid8 NOT NULL DEFAULT pg_current_xact_id(),
    key              text NOT NULL,
    seq              bigint NOT NULL,
    at               timestamptz NOT NULL,
    kind             text NOT NULL,
    severity         text NOT NULL CHECK (severity IN ('info', 'warning', 'critical')),
    organization_id  uuid NOT NULL,
    workload_id      uuid,
    effect_id        uuid,
    detail           jsonb NOT NULL DEFAULT '{}',
    UNIQUE (key, seq)
);
CREATE INDEX event_unpositioned ON event (id) WHERE position IS NULL;
CREATE INDEX event_at ON event (at);

-- The next position to assign, in one row the sequencer locks.
CREATE TABLE event_sequencer (
    only_row  boolean PRIMARY KEY DEFAULT true CHECK (only_row),
    next      bigint NOT NULL CHECK (next > 0)
);
INSERT INTO event_sequencer (next) VALUES (1);

-- Writes an event for the effect just inserted. An effect that concerns no organization (the
-- installation's own bootstrap, before any organization exists to read it) has no event.
CREATE FUNCTION korpis_effect_event() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    org      uuid;
    k        text;
    n        bigint;
BEGIN
    org := coalesce(
        (SELECT organization_id FROM workload WHERE id = NEW.workload_id),
        (SELECT organization_id FROM node WHERE id = NEW.node_id),
        (SELECT korpis_object_organization(scope_kind, scope_id) FROM grants
          WHERE id = NEW.grant_id));
    IF org IS NULL THEN
        RETURN NULL;
    END IF;
    k := CASE WHEN NEW.workload_id IS NOT NULL THEN 'workload:' || NEW.workload_id
              ELSE 'organization:' || org END;
    INSERT INTO event_key (key, seq) VALUES (k, 1)
        ON CONFLICT (key) DO UPDATE SET seq = event_key.seq + 1
        RETURNING seq INTO n;
    INSERT INTO event (key, seq, at, kind, severity, organization_id, workload_id, effect_id,
                       detail)
    VALUES (k, n, NEW.at, NEW.action,
            CASE WHEN NEW.outcome IN ('succeeded', 'skipped') THEN 'info' ELSE 'warning' END,
            org, NEW.workload_id, NEW.id,
            coalesce(NEW.after, '{}'::jsonb) || coalesce(NEW.error, '{}'::jsonb));
    PERFORM pg_notify('korpis_event_written', '');
    RETURN NULL;
END
$$;
CREATE TRIGGER effect_event AFTER INSERT ON effect FOR EACH ROW EXECUTE FUNCTION korpis_effect_event();

-- Positions up to `batch` events whose transactions have all ended, in insertion order;
-- returns how many. One sequencer runs at a time, on the row lock.
CREATE FUNCTION korpis_sequence_events(batch integer) RETURNS bigint
LANGUAGE plpgsql AS $$
DECLARE
    horizon  xid8 := pg_snapshot_xmin(pg_current_snapshot());
    first    bigint;
    n        bigint;
BEGIN
    SELECT next INTO first FROM event_sequencer FOR UPDATE;
    WITH ready AS (
        SELECT id, first - 1 + row_number() OVER (ORDER BY id) AS pos
          FROM (SELECT id FROM event WHERE position IS NULL AND xid < horizon
                 ORDER BY id LIMIT batch) r
    )
    UPDATE event SET position = ready.pos FROM ready WHERE event.id = ready.id;
    GET DIAGNOSTICS n = ROW_COUNT;
    IF n > 0 THEN
        UPDATE event_sequencer SET next = first + n;
        PERFORM pg_notify('korpis_event_positioned', (first + n - 1)::text);
    END IF;
    RETURN n;
END
$$;

GRANT SELECT, INSERT, UPDATE ON event_key TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON event TO korpis_app;
GRANT SELECT, UPDATE ON event_sequencer TO korpis_app;
GRANT EXECUTE ON FUNCTION korpis_sequence_events(integer) TO korpis_app;
