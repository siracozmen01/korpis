-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Whether a migration moves its workload's execution state: its quiesce captures the state rather
-- than stopping the workload, and its destination resumes it rather than starting it, as the plan
-- approved said. Every migration made before moved none.

ALTER TABLE migration ADD COLUMN live boolean NOT NULL DEFAULT false;
