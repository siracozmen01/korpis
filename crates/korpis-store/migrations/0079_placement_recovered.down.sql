-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0079. A recovered placement stays where it is, recorded as the scheduler's.

UPDATE placement SET bound_by = 'scheduler' WHERE bound_by = 'recovery';
ALTER TABLE placement DROP COLUMN recovered_epoch;
ALTER TABLE placement DROP CONSTRAINT placement_bound_by_check;
ALTER TABLE placement
    ADD CONSTRAINT placement_bound_by_check
        CHECK (bound_by IN ('scheduler', 'operator', 'migration'));
