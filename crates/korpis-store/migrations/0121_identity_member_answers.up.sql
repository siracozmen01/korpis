-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- How many times each member's record was written (§5.1 of docs/08-identity.md, Finding 141 of
-- docs/23-walkthroughs.md): a set's re-verification reads it when it asks, and records that a
-- member is still in the set only if no sign-in or other answer wrote their record meanwhile, so
-- an older answer never replaces a newer one, as a binding's own count of answers already keeps
-- for the binding.

ALTER TABLE identity_member ADD COLUMN answers bigint NOT NULL DEFAULT 0 CHECK (answers >= 0);
