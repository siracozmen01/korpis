-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Sets organization $1's first policy, at version $5; nothing if it already has one.
INSERT INTO trust_policy (organization_id, accepted_signers, allow_unsigned, allow_unverified,
                          version, set_by, set_at)
VALUES ($1, $2, $3, $4, $5, $6, $7)
ON CONFLICT (organization_id) DO NOTHING
