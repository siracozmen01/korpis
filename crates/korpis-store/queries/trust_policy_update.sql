-- Replaces organization $1's policy, or its removal, at version $5, if it is still at version $8.
-- A removal is $9: the policy columns are then cleared and the ancestors' policy governs.
UPDATE trust_policy SET accepted_signers = $2, allow_unsigned = $3, allow_unverified = $4,
                        version = $5, set_by = $6, set_at = $7, inherit = $9
 WHERE organization_id = $1 AND version = $8
