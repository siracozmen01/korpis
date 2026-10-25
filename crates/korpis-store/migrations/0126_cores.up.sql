-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A node's physical cores offered for dedication, reported by its agent in full; and the cores a
-- placement holds, chosen when it was bound (§5.2 of docs/05-scheduling.md). A node reports none
-- until its agent reads its topology, and no placement held one before.

ALTER TABLE node ADD COLUMN cores jsonb NOT NULL DEFAULT '[]';
ALTER TABLE placement ADD COLUMN cores integer[] NOT NULL DEFAULT '{}';
