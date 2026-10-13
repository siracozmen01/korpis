-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Each organization's recipe trust policy (§7 of docs/09-recipes.md). An organization without one
-- uses the nearest ancestor's, and with none anywhere the default runs nothing. Signers are kept
-- with their PEM key and its fingerprint; the fingerprint is the identity a signature is checked
-- against. An organization that removes its policy keeps its row, marked `inherit`, so the version
-- keeps counting and a client holding an old version cannot overwrite a newer policy.

CREATE TABLE trust_policy (
    organization_id   uuid PRIMARY KEY REFERENCES organization (id),
    accepted_signers  jsonb NOT NULL DEFAULT '[]',
    allow_unsigned    boolean NOT NULL DEFAULT false,
    allow_unverified  boolean NOT NULL DEFAULT false,
    inherit           boolean NOT NULL DEFAULT false,
    version           bigint NOT NULL CHECK (version > 0),
    set_by            text NOT NULL,
    set_at            timestamptz NOT NULL
);

GRANT SELECT, INSERT, UPDATE ON trust_policy TO korpis_app;
