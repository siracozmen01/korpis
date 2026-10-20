-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- When each identity binding is next re-verified (§5.1 of docs/08-identity.md). A control plane
-- claims the due bindings by moving this forward for as long as one verification may take, so
-- two replicas never ask a provider the same question at once, and one that dies mid-question
-- leaves the binding due again shortly after. A binding is due as soon as it is made. Each answer
-- recorded counts one more verification, and is recorded only against the count it was asked at,
-- so an answer that arrives after a newer one never replaces it.

ALTER TABLE identity_binding ADD COLUMN verify_due_at timestamptz NOT NULL DEFAULT now();
ALTER TABLE identity_binding ADD COLUMN verifications bigint NOT NULL DEFAULT 0
    CHECK (verifications >= 0);
CREATE INDEX identity_binding_due ON identity_binding (verify_due_at) WHERE retired_at IS NULL;
