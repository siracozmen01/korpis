-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Principals are stored as their canonical key, not as a wire message.
--
-- The first schema kept a grant's subject and issuer, and an effect's actor, as the JSON of the
-- korpis.v1.Subject message. That tied stored data to the wire schema: a field renamed in the
-- protocol would have needed a data migration of the audit log, which is append-only and must
-- never be rewritten. The canonical key (`token:<uuid>`, `ext:discord:set:<guild>:<role>`) is
-- defined by the domain, parses back to exactly one principal, and is what grants are already
-- indexed by, so it replaces the JSON everywhere and `grants.subject` duplicated `subject_key`.

ALTER TABLE grants DROP COLUMN subject;
ALTER TABLE grants ALTER COLUMN issued_by TYPE text USING issued_by #>> '{}';
ALTER TABLE effect ALTER COLUMN actor TYPE text USING actor #>> '{}';
