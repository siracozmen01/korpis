-- SPDX-License-Identifier: AGPL-3.0-or-later

ALTER TABLE metering_record DROP COLUMN device_seconds, DROP COLUMN dedicated_core_seconds;
ALTER TABLE metering_reading DROP COLUMN device_seconds, DROP COLUMN dedicated_core_seconds;
