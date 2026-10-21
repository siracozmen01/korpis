-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Metering (§2 of docs/15-observability.md, "Measuring it, as designed"). A node's sample carries
-- a workload's counters since it first ran there, so the control plane keeps, for each workload
-- on each node, the last sample it took, and writes what each interval used as the difference
-- from it. A record is never changed: a gap filled, a late sample, and a correction are each a
-- new record naming the one it supersedes, and the current value is the newest.

-- The last sample taken of each workload on each node: where its next is measured from.
CREATE TABLE metering_reading (
    workload_id         uuid NOT NULL REFERENCES workload (id),
    node_id             uuid NOT NULL REFERENCES node (id),
    interval_end        timestamptz NOT NULL,
    cpu_usec            bigint,
    cpu_throttled_usec  bigint,
    io_read_bytes       bigint,
    io_write_bytes      bigint,
    net_rx_bytes        bigint,
    net_tx_bytes        bigint,
    memory_kib_seconds  bigint,
    PRIMARY KEY (workload_id, node_id)
);
GRANT SELECT, INSERT, UPDATE ON metering_reading TO korpis_app;

-- What each interval, or each span of lost samples, used. Partitioned by when it was recorded,
-- as the effect log is, so a record of an interval long past still lands in a partition that
-- exists, and read by its workload and interval.
CREATE TABLE metering_record (
    id                  uuid NOT NULL,
    recorded_at         timestamptz NOT NULL,
    workload_id         uuid NOT NULL,
    node_id             uuid,
    lease_epoch         bigint NOT NULL DEFAULT 0,
    interval_start      timestamptz NOT NULL,
    interval_end        timestamptz NOT NULL,
    kind                text NOT NULL CHECK (kind IN ('measured', 'gap', 'adjustment')),
    cause               text,
    supersedes          uuid,
    cpu_usec            bigint,
    cpu_throttled_usec  bigint,
    io_read_bytes       bigint,
    io_write_bytes      bigint,
    net_rx_bytes        bigint,
    net_tx_bytes        bigint,
    memory_kib_seconds  bigint,
    memory_peak_bytes   bigint,
    disk_bytes          bigint,
    losses              jsonb NOT NULL DEFAULT '[]',
    PRIMARY KEY (recorded_at, id),
    CHECK (interval_end > interval_start)
) PARTITION BY RANGE (recorded_at);
CREATE INDEX metering_record_workload ON metering_record (workload_id, interval_start);

-- Monthly partitions, created ahead of need with the effect log's.
CREATE FUNCTION korpis_ensure_metering_partitions(months_ahead integer) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public AS $$
DECLARE
    month_start date := date_trunc('month', now() - interval '1 month')::date;
    i integer;
    part text;
BEGIN
    FOR i IN 0 .. months_ahead + 1 LOOP
        part := format('metering_record_%s',
            to_char(month_start + make_interval(months => i), 'YYYY_MM'));
        IF to_regclass(part) IS NULL THEN
            EXECUTE format(
                'CREATE TABLE %I PARTITION OF metering_record FOR VALUES FROM (%L) TO (%L)',
                part,
                month_start + make_interval(months => i),
                month_start + make_interval(months => i + 1));
            EXECUTE format('REVOKE UPDATE, DELETE, TRUNCATE ON %I FROM korpis_app', part);
            EXECUTE format('GRANT SELECT, INSERT ON %I TO korpis_app', part);
        END IF;
    END LOOP;
END
$$;
SELECT korpis_ensure_metering_partitions(3);
GRANT SELECT, INSERT ON metering_record TO korpis_app;
GRANT EXECUTE ON FUNCTION korpis_ensure_metering_partitions(integer) TO korpis_app;
