-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- What a workload's placement held, for the time it held it (§2 of docs/15-observability.md):
-- each dedicated core and each device, whatever ran on them, integrated by the node, cumulative
-- in a reading and the differences in a record, as the other counters are. `device_seconds` is
-- each class held, once: [{"class": "gpu", "seconds": 7200}].

ALTER TABLE metering_reading
    ADD COLUMN dedicated_core_seconds bigint,
    ADD COLUMN device_seconds jsonb NOT NULL DEFAULT '[]';
ALTER TABLE metering_record
    ADD COLUMN dedicated_core_seconds bigint,
    ADD COLUMN device_seconds jsonb NOT NULL DEFAULT '[]';
