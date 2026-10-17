-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A recipe's catalogues (§5.3 of docs/09-recipes.md), kept with its document so a recipe read
-- back from the store speaks every language it was published in without asking its registry
-- again. An object from a locale to an object from a message's key to its translation; checked
-- against the document whenever it is read, since the rules may have grown. A recipe resolved
-- before catalogues were read keeps none, and shows its own messages.

ALTER TABLE recipe
    ADD COLUMN catalogues jsonb NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(catalogues) = 'object');
