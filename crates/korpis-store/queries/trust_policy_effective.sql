-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The trust policy governing organization $1: its own, or the nearest ancestor's, skipping any
-- organization that removed its own.
WITH RECURSIVE up AS (
    SELECT id, parent_id, 0 AS depth FROM organization WHERE id = $1
    UNION ALL
    SELECT o.id, o.parent_id, up.depth + 1 FROM organization o JOIN up ON o.id = up.parent_id
)
SELECT t.organization_id, t.accepted_signers, t.allow_unsigned, t.allow_unverified, t.version,
       t.set_by, t.set_at
  FROM up JOIN trust_policy t ON t.organization_id = up.id AND NOT t.inherit
 ORDER BY up.depth
 LIMIT 1
