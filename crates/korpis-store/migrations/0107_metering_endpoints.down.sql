-- SPDX-License-Identifier: AGPL-3.0-or-later

ALTER TABLE metering_record DROP COLUMN endpoints, DROP COLUMN egress_bytes;
ALTER TABLE metering_reading DROP COLUMN endpoints, DROP COLUMN egress_bytes;
