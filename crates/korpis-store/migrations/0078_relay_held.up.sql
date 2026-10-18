-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Relays held by a restore (§8 of docs/03-state.md, §6 of docs/18-operations.md). A restore
-- cannot know what a relay the backup records as joined has been fed since, nor whether its
-- identity is still the one that joined, so it holds every one: a held relay's feed is answered
-- as unavailable and it is named to no node, until an operator readmits it or removes it.

ALTER TABLE relay ADD COLUMN held_at timestamptz;
