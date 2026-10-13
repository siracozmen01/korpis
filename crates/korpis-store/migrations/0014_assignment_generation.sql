-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Assignments keep their generation for good (§4.8 of docs/02-architecture.md). The generation is
-- an assignment's identity, and it must only ever rise: an assignment that leaves a node's set is
-- kept, inactive, so a workload placed there again continues from the generation it had rather
-- than starting at one, which an agent holding the old generation would read as going backward.
-- `content_digest` is over the assignment with its generation zeroed, so recomputing an unchanged
-- assignment is recognized as unchanged and neither re-signed nor re-sent.
--
-- A node records the set digest it last acknowledged, for convergence (§4.3).

ALTER TABLE assignment
    ADD COLUMN content_digest bytea NOT NULL DEFAULT '\x',
    ADD COLUMN active boolean NOT NULL DEFAULT true;

ALTER TABLE node
    ADD COLUMN acked_set_digest text,
    ADD COLUMN acked_at timestamptz;
