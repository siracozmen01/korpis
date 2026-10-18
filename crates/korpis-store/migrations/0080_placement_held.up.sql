-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Placements a restore holds (§8 of docs/03-state.md, Finding 97 of docs/23-walkthroughs.md). A
-- workload retired after the backup is live and placed in the restored store, and its node, which
-- removed it, would be told to run it again. A node presenting its assignments without one the
-- store places on it holds that placement: nothing is sent for it until an operator confirms it or
-- it is retired.

ALTER TABLE placement ADD COLUMN held_at timestamptz;
