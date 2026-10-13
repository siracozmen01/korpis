-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The sequencer positions an event only when no earlier event of its key still waits (§5 of
-- docs/15-observability.md). Eligibility is by transaction identifier against the cluster's
-- snapshot horizon, and identifiers are taken at a transaction's first write, not at its commit:
-- a key's second event can come from a transaction with the smaller identifier, one that took
-- the key's row only after the first event's transaction committed. With an unrelated
-- transaction open whose identifier falls between the two, the second event is below the
-- horizon and the first is not, and positioning what is eligible would give the key's second
-- event the smaller position. Holding an event back while its key has an earlier one waiting
-- keeps every key's positions in its commit order; other keys are not held back.

CREATE INDEX event_waiting_by_key ON event (key, seq) WHERE position IS NULL;

CREATE OR REPLACE FUNCTION korpis_sequence_events(batch integer) RETURNS bigint
LANGUAGE plpgsql AS $$
DECLARE
    horizon  xid8 := pg_snapshot_xmin(pg_current_snapshot());
    first    bigint;
    n        bigint;
BEGIN
    SELECT next INTO first FROM event_sequencer FOR UPDATE;
    WITH ready AS (
        SELECT id, first - 1 + row_number() OVER (ORDER BY id) AS pos
          FROM (SELECT e.id FROM event e
                 WHERE e.position IS NULL AND e.xid < horizon
                   AND NOT EXISTS (SELECT 1 FROM event w
                                    WHERE w.key = e.key AND w.position IS NULL
                                      AND w.seq < e.seq AND w.xid >= horizon)
                 ORDER BY e.id LIMIT batch) r
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
