-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Power has a head and a target, like intents (§3.3 of docs/01-model.md).
--
-- The workload kept one power pointer, `power_version`, moved at declaration. A node is told the
-- workload's power in its assignment (§4.8 of docs/02-architecture.md), so a stop that waits for
-- confirmation, because its recipe declares stopping disruptive, would have reached the node the
-- moment it was declared: finding S3 again, for power instead of configuration. The newest
-- declaration stays in `power_version`; what nodes are told is `power_target_version`, moved only
-- by approving a plan, by compare-and-swap. Zero means nothing is approved yet.

ALTER TABLE workload
    ADD COLUMN power_target_version bigint NOT NULL DEFAULT 0,
    ADD CONSTRAINT workload_power_target_declared
        CHECK (power_target_version >= 0 AND power_target_version <= power_version);

-- A plan records the power target it was computed against, as it records `from_intent`, so that
-- a plan cancelled before any of its steps completed can put both back (§7 of
-- docs/03-state.md).
ALTER TABLE plan ADD COLUMN from_power bigint NOT NULL DEFAULT 0 CHECK (from_power >= 0);
ALTER TABLE plan ALTER COLUMN from_power DROP DEFAULT;
