-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A migration's passes, as the control plane starts them (§8.1 of docs/05-scheduling.md): each
-- volume's pass under way names the pass it is an increment from, which the destination holds,
-- so both nodes' roles carry the pass's ticket and the source sends the increment the ticket
-- names. The control plane names every pass, so a pass's identifier is what both nodes report it
-- by, and a report of any other is of no pass under way.

ALTER TABLE migration_volume ADD COLUMN parent_pass uuid;
