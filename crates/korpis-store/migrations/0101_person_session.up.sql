-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A session held by a person at an identity provider rather than by an account (§9.7 of
-- docs/08-identity.md): it names the person's own binding, the one live binding of their
-- subject, and keeps the sets the provider named at their sign-in, which the gateway reads at
-- each call and evaluation holds only while the person's record as a member of each vouches. A
-- session is held by an account or by a person, never both and never neither.

ALTER TABLE user_session ALTER COLUMN account_id DROP NOT NULL;
ALTER TABLE user_session
    ADD COLUMN binding_id uuid REFERENCES identity_binding (id) ON DELETE CASCADE,
    -- The sets of the sign-in, as `[{"realm": .., "member_of": ..}]`.
    ADD COLUMN sets jsonb NOT NULL DEFAULT '[]'::jsonb,
    ADD CONSTRAINT user_session_held CHECK ((account_id IS NULL) <> (binding_id IS NULL)),
    ADD CONSTRAINT user_session_sets CHECK (jsonb_typeof(sets) = 'array'
        AND (binding_id IS NOT NULL OR sets = '[]'::jsonb));
CREATE INDEX user_session_binding ON user_session (binding_id) WHERE binding_id IS NOT NULL;
