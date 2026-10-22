-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The placement policy an organization sets for its subtree (§2.2 of docs/05-scheduling.md,
-- question 8 of §10 there): its strategy and weights score the nodes its subtree's
-- organizations own where none of them sets one nearer, and its spread constraints and affinity
-- rules place the workloads they hold. Null where it sets none, and the installation's applies.

ALTER TABLE organization ADD COLUMN placement_policy jsonb
    CHECK (placement_policy IS NULL OR jsonb_typeof(placement_policy) = 'object');
