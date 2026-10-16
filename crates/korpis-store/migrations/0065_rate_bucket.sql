-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Rate limit buckets (§10 of docs/10-api.md): each a generic cell rate, kept as the instant the
-- bucket would be full again. Not written to the log: every replica charges the same rows, and a
-- crash forgives what was owed rather than adding to it.
--
-- korpis_rate_charge charges a request to every bucket it names, all or none: each row is locked
-- in key order, so two requests sharing buckets never wait on each other in a cycle; a request
-- admitted by every bucket moves each on by its interval, and one any bucket refuses moves none.
-- It answers, per bucket, whether it admitted the request and when it will be full again.

CREATE UNLOGGED TABLE rate_bucket (
    key      text PRIMARY KEY,
    full_at  timestamptz NOT NULL
);

CREATE FUNCTION korpis_rate_charge(
    keys       text[],
    intervals  bigint[],
    bursts     integer[],
    now        timestamptz
) RETURNS TABLE (key text, admitted boolean, full_at timestamptz)
LANGUAGE plpgsql AS $$
#variable_conflict use_column
DECLARE
    i      integer;
    h      timestamptz;
    held   timestamptz[];
    next   timestamptz[];
    ok     boolean[];
    all_ok boolean := true;
    step   interval;
BEGIN
    INSERT INTO rate_bucket (key, full_at)
    SELECT k, now FROM unnest(keys) AS k ORDER BY k
    ON CONFLICT DO NOTHING;
    PERFORM 1 FROM rate_bucket b WHERE b.key = ANY (keys) ORDER BY b.key FOR UPDATE;
    FOR i IN 1 .. coalesce(array_length(keys, 1), 0) LOOP
        step := make_interval(secs => intervals[i] / 1000000.0);
        SELECT greatest(b.full_at, now) INTO h FROM rate_bucket b WHERE b.key = keys[i];
        held[i] := h;
        next[i] := held[i] + step;
        ok[i] := next[i] - step * bursts[i] <= now;
        all_ok := all_ok AND ok[i];
    END LOOP;
    FOR i IN 1 .. coalesce(array_length(keys, 1), 0) LOOP
        IF all_ok THEN
            UPDATE rate_bucket b SET full_at = next[i] WHERE b.key = keys[i];
            key := keys[i]; admitted := true; full_at := next[i];
        ELSE
            key := keys[i]; admitted := ok[i]; full_at := held[i];
        END IF;
        RETURN NEXT;
    END LOOP;
END;
$$;

GRANT SELECT, INSERT, UPDATE, DELETE ON rate_bucket TO korpis_app;
GRANT EXECUTE ON FUNCTION korpis_rate_charge(text[], bigint[], integer[], timestamptz)
    TO korpis_app;
