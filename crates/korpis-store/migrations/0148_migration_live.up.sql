-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Whether a migration moves its workload's execution state: its quiesce captures the state rather
-- than stopping the workload, and its destination resumes it rather than starting it, as the plan
-- approved said. Every migration made before moved none. The state crosses in a volume of the
-- workload's of its own kind, `state`, as the console stream's crosses in one of kind `stream`.

ALTER TABLE migration ADD COLUMN live boolean NOT NULL DEFAULT false;
ALTER TABLE volume DROP CONSTRAINT volume_kind_check;
ALTER TABLE volume ADD CONSTRAINT volume_kind_check
    CHECK (kind IN ('declared', 'stream', 'state'));
