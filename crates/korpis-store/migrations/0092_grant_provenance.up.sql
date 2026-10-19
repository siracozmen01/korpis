-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A grant issued from a template names the template and version it came from, as provenance
-- that no authorization decision reads (§3.5 and §4 of docs/08-identity.md). The first schema
-- kept it as a document no grant ever filled; it is two columns now, the pair a foreign key to
-- the version kept whole, so a grant's provenance always names what was issued and the grants
-- of one template are found by an index rather than a scan.

ALTER TABLE grants DROP COLUMN from_template;
ALTER TABLE grants ADD COLUMN template_id uuid;
ALTER TABLE grants ADD COLUMN template_version bigint;
ALTER TABLE grants ADD CONSTRAINT grants_from_template
    FOREIGN KEY (template_id, template_version)
    REFERENCES grant_template_version (template_id, version);
ALTER TABLE grants ADD CONSTRAINT grants_template_whole
    CHECK ((template_id IS NULL) = (template_version IS NULL));
CREATE INDEX grants_template ON grants (template_id, id) WHERE template_id IS NOT NULL;
