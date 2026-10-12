-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The cosign signatures found beside each recipe (§7 of docs/09-recipes.md), kept like the recipe
-- itself: an organization's policy is evaluated against them whenever a workload is declared, and
-- a rollback to a years-old intent must be verifiable without the registry still holding them.
-- Whose they are is not recorded: a signature is checked against the keys a policy accepts when it
-- is evaluated, never trusted for having been stored. Rows are immutable.

CREATE TABLE recipe_signature (
    digest     text NOT NULL CHECK (digest ~ '^sha256:[0-9a-f]{64}$'),
    payload    bytea NOT NULL,
    signature  text NOT NULL,
    found_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (digest, signature)
);

GRANT SELECT, INSERT ON recipe_signature TO korpis_app;
