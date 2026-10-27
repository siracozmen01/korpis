-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- What a block class's device holds, fixed when it is wanted (§2.1 and §3.1 of
-- docs/06-storage.md): a filesystem the host mounts for a workload of files, or a guest's disk,
-- which only the guest's kernel parses and the host never mounts. A device of one kind is never
-- given as the other.

ALTER TABLE block_device ADD COLUMN guest boolean NOT NULL DEFAULT false;
