-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Every recipe ever resolved, by digest (§8 of docs/09-recipes.md). An intent names a digest and
-- nothing else, and rolling back to an old intent must not depend on a registry still holding
-- the recipe, so the document is kept here, as published, for as long as an intent may name it.
-- Rows are immutable: a digest names exactly one document.

CREATE TABLE recipe (
    digest       text PRIMARY KEY CHECK (digest ~ '^sha256:[0-9a-f]{64}$'),
    name         text NOT NULL,
    -- The version that first resolved to this digest; later ones may differ.
    version      text NOT NULL,
    document     bytea NOT NULL,
    resolved_at  timestamptz NOT NULL DEFAULT now()
);

GRANT SELECT, INSERT ON recipe TO korpis_app;
