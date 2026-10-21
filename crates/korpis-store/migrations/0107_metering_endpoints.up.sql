-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Bytes by endpoint, and egress (§2 of docs/15-observability.md): what the host's table counted
-- of a workload's link, cumulative in a reading and the differences in a record, as the other
-- counters are. Each endpoint whose accounting is on is a member of `endpoints`, by its name:
-- {"game": {"received_bytes": 1200, "received_packets": 4, "sent_bytes": 4800,
-- "sent_packets": 6}}. A record is never changed, so its endpoints are read with it rather than
-- joined from a table of their own.

ALTER TABLE metering_reading
    ADD COLUMN egress_bytes bigint,
    ADD COLUMN endpoints jsonb NOT NULL DEFAULT '{}';
ALTER TABLE metering_record
    ADD COLUMN egress_bytes bigint,
    ADD COLUMN endpoints jsonb NOT NULL DEFAULT '{}';
