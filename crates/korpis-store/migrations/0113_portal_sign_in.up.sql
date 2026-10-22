-- SPDX-License-Identifier: AGPL-3.0-or-later

-- A sign-in through an identity provider at a portal (§9.8 of docs/08-identity.md) comes back to
-- the portal's own callback, so a pending sign-in records the redirect URI it was sent with, which
-- the code is redeemed with, and the portal it began at, where alone it is completed. A pending
-- sign-in lasts ten minutes; those begun before this migration knew neither, and are let go, as
-- an expiry would.

DELETE FROM pending_sign_in;
ALTER TABLE pending_sign_in
    ADD COLUMN redirect_uri text NOT NULL,
    ADD COLUMN portal_id uuid REFERENCES portal (id) ON DELETE CASCADE;
