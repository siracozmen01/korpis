-- SPDX-License-Identifier: AGPL-3.0-or-later

-- Signing in at a portal (§9.8 of docs/08-identity.md). A session records the portal it began at,
-- beneath whose organization it acts; a passkey, the portal whose name is its relying party's
-- identifier, the only address it signs in at; a challenge, the portal it was issued at, whose
-- relying party its answer is verified for. None is the installation's own address. Each goes
-- with its portal: deleting one ends what began there (the control plane records each passkey's
-- removal before it deletes the portal, so the cascade only keeps a row from outliving it).

ALTER TABLE user_session ADD COLUMN portal_id uuid REFERENCES portal (id) ON DELETE CASCADE;
CREATE INDEX user_session_portal ON user_session (portal_id) WHERE portal_id IS NOT NULL;

ALTER TABLE passkey ADD COLUMN portal_id uuid REFERENCES portal (id) ON DELETE CASCADE;
CREATE INDEX passkey_portal ON passkey (portal_id) WHERE portal_id IS NOT NULL;

ALTER TABLE webauthn_challenge
    ADD COLUMN portal_id uuid REFERENCES portal (id) ON DELETE CASCADE;
