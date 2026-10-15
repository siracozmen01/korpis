-- SPDX-License-Identifier: AGPL-3.0-or-later
-- A workload whose install resolved to whatever a channel held (§4 of docs/09-recipes.md): when it
-- first did, and what. Set once and never cleared; null while the workload can be reproduced
-- from its recipe alone.
ALTER TABLE workload
    ADD COLUMN unreproducible_at     timestamptz,
    ADD COLUMN unreproducible_reason text,
    ADD CONSTRAINT workload_unreproducible_whole
        CHECK ((unreproducible_at IS NULL) = (unreproducible_reason IS NULL));
