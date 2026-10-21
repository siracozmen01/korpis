-- SPDX-License-Identifier: AGPL-3.0-or-later

DROP TABLE metering_period;
ALTER TABLE metering_reading DROP COLUMN gapped_through;
DROP INDEX metering_record_gaps;
DROP INDEX metering_record_supersedes;
ALTER TABLE metering_record
    ALTER COLUMN supersedes DROP NOT NULL,
    ALTER COLUMN supersedes DROP DEFAULT,
    ALTER COLUMN supersedes TYPE uuid USING supersedes[1];
