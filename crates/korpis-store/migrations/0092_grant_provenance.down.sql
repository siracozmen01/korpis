-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0092. What the two columns held is not carried back: the document they
-- replaced was never read.

DROP INDEX grants_template;
ALTER TABLE grants DROP CONSTRAINT grants_template_whole;
ALTER TABLE grants DROP CONSTRAINT grants_from_template;
ALTER TABLE grants DROP COLUMN template_version;
ALTER TABLE grants DROP COLUMN template_id;
ALTER TABLE grants ADD COLUMN from_template jsonb;
