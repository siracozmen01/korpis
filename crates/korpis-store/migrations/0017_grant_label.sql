-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- What a grant is for, for the people who read it later (§3.7 of docs/08-identity.md). No
-- authorization decision reads it. Bounded so that a listing stays a listing.

ALTER TABLE grants ADD COLUMN label text NOT NULL DEFAULT '';
ALTER TABLE grants ADD CONSTRAINT grants_label_bounded CHECK (char_length(label) <= 200);
