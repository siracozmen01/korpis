-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The delegation ceiling an organization sets for its subtree (§5.1 of docs/08-identity.md):
-- the longest a grant beneath a set subject lasts there, counted from its issue. Authorization
-- takes the lowest along a scope's organizations, seven days where none sets one; each grant
-- keeps when it was issued already (grants.issued_at).

ALTER TABLE organization ADD COLUMN delegation_ceiling_ms bigint
    CHECK (delegation_ceiling_ms > 0);
