-- SPDX-License-Identifier: AGPL-3.0-or-later

DROP INDEX volume_divergent_of;
ALTER TABLE volume DROP CONSTRAINT volume_divergent;
ALTER TABLE volume DROP COLUMN promoting;
ALTER TABLE volume DROP COLUMN held_until;
ALTER TABLE volume DROP COLUMN divergent_since;
ALTER TABLE volume DROP COLUMN divergent_of;
