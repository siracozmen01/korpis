-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Placements a restore recovers (§8 of docs/03-state.md, Finding 96 of docs/23-walkthroughs.md).
-- A workload recovered from what its node presented, or one the backup places on a node it has
-- since left, would be placed again by the scheduler while its node still runs it. Recovery binds
-- it to the node that presented it, and where two nodes present one workload, to the one whose
-- assignment names the higher lease epoch. That epoch is kept with the placement, whoever bound
-- it, so a node that presents later at a lower one does not take it.

ALTER TABLE placement DROP CONSTRAINT placement_bound_by_check;
ALTER TABLE placement
    ADD CONSTRAINT placement_bound_by_check
        CHECK (bound_by IN ('scheduler', 'operator', 'migration', 'recovery')),
    ADD COLUMN recovered_epoch bigint CHECK (recovered_epoch >= 0);
