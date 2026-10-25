-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A node's devices, as the Container Device Interface's files on it describe them, and those it
-- does not offer, both reported by its agent in full; and the devices a placement holds, chosen
-- when it was bound (§6 of docs/05-scheduling.md). A node reports none until its agent reads the
-- files, and no placement held one before.

ALTER TABLE node ADD COLUMN devices jsonb NOT NULL DEFAULT '[]',
                 ADD COLUMN unoffered_devices jsonb NOT NULL DEFAULT '[]';
ALTER TABLE placement ADD COLUMN devices jsonb NOT NULL DEFAULT '[]';
