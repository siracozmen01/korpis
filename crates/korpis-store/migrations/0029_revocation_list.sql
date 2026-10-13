-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The revocation list (§6.3 of docs/08-identity.md) is recomputed every few seconds by every
-- replica from the grants revoked or suspended within the longest token lifetime. These indexes
-- keep that a range scan over the few recent entries rather than a pass over every grant.

CREATE INDEX grants_revoked_at ON grants (revoked_at) WHERE revoked_at IS NOT NULL;
CREATE INDEX grants_suspended_at ON grants (suspended_at)
    WHERE suspended_at IS NOT NULL AND revoked_at IS NULL;
