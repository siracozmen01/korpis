-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Root grants are marked, not inferred from a missing parent. A grant has no parent exactly when
-- it is a root, and roots are created only at installation and by local root recovery (§9.1 and
-- §9.2 of docs/08-identity.md). Evaluation refuses a parentless grant that is not a root, so a
-- row inserted without a parent by mistake roots nothing.

ALTER TABLE grants ADD COLUMN root boolean NOT NULL DEFAULT false;
ALTER TABLE grants ADD CONSTRAINT grants_root_iff_parentless CHECK ((parent_id IS NULL) = root);
