-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- What a node runs for a workload, beside the assignment that tells it (§4.3 of
-- docs/02-architecture.md): a digest of the assignment without its plan and its ACME challenges,
-- and the generation at which that last changed. A workload's convergence compares what its node
-- has seen with this generation, so an assignment that only drops a finished plan, or carries a
-- certificate's challenge for a minute, does not show a converged workload as applying again;
-- a plan is measured by its own progress.

ALTER TABLE assignment
    ADD COLUMN runs_digest bytea NOT NULL DEFAULT '',
    ADD COLUMN runs_generation bigint;

UPDATE assignment SET runs_generation = generation;

ALTER TABLE assignment ALTER COLUMN runs_generation SET NOT NULL;
