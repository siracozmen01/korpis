-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The members of a bound set who issued a grant through it (§5.1 of docs/08-identity.md): a
-- bounded set, members who delegated rather than members who exist. Each is asked about with
-- the set at the set binding's re-verification, and what they delegated stands only while the
-- last answer said they are in it.

CREATE TABLE identity_member (
    binding_id        uuid NOT NULL REFERENCES identity_binding (id),
    -- The member's principal key.
    member            text NOT NULL,
    state             text NOT NULL CHECK (state IN ('verified', 'unverified')),
    last_verified_at  timestamptz,
    recorded_at       timestamptz NOT NULL,
    PRIMARY KEY (binding_id, member)
);

GRANT SELECT, INSERT, UPDATE, DELETE ON identity_member TO korpis_app;
