-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A bucket full again is forgotten (Finding 69 of docs/23-walkthroughs.md), since a bucket kept
-- nowhere is full: so a bucket costs a row only while it owes, and a flood of new addresses or
-- invented sign-in names leaves no rows behind once their buckets refill. A charge may meet a
-- bucket forgotten between its insert and its lock; it then reads the bucket as full, which it
-- is, and writes it back by upsert. Two charges racing that one forgetting can each read it full
-- and the later write wins, so at most one request is admitted uncounted, and only for a bucket
-- that owed nothing.

CREATE OR REPLACE FUNCTION korpis_rate_charge(
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
        held[i] := coalesce(h, now);
        next[i] := held[i] + step;
        ok[i] := next[i] - step * bursts[i] <= now;
        all_ok := all_ok AND ok[i];
    END LOOP;
    FOR i IN 1 .. coalesce(array_length(keys, 1), 0) LOOP
        IF all_ok THEN
            INSERT INTO rate_bucket AS b (key, full_at) VALUES (keys[i], next[i])
            ON CONFLICT (key) DO UPDATE SET full_at = EXCLUDED.full_at;
            key := keys[i]; admitted := true; full_at := next[i];
        ELSE
            key := keys[i]; admitted := ok[i]; full_at := held[i];
        END IF;
        RETURN NEXT;
    END LOOP;
END;
$$;

CREATE INDEX rate_bucket_full ON rate_bucket (full_at);
