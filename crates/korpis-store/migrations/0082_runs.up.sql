-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Runs, the executions of a task or scheduled workload, and what the control plane keeps of each
-- such workload beside them (§4 of docs/05-scheduling.md). At most one run is made for one
-- occurrence of a workload's schedule, and one for one power cycle, so two control planes
-- evaluating the same minute make one run. A run ends once, and only an active run (pending or
-- running) changes. run_state holds the next occurrence and the highest cycle a run was made for,
-- so that pruning a run never makes it again.

CREATE TABLE run (
    id             uuid PRIMARY KEY,
    workload_id    uuid NOT NULL REFERENCES workload (id),
    intent_id      uuid NOT NULL REFERENCES intent (id),
    trigger        text NOT NULL CHECK (trigger IN ('schedule', 'manual', 'retry', 'event')),
    scheduled_for  timestamptz NOT NULL,
    started_at     timestamptz,
    ended_at       timestamptz,
    node_id        uuid REFERENCES node (id),
    status         text NOT NULL
                   CHECK (status IN ('pending', 'running', 'succeeded', 'failed', 'cancelled',
                                     'missed')),
    exit_status    integer,
    attempt        integer NOT NULL CHECK (attempt >= 1),
    cycle          bigint CHECK (cycle >= 0),
    occurrences    integer NOT NULL CHECK (occurrences >= 1),
    cause          text CHECK (cause IN ('exit', 'timeout', 'cancelled', 'stopped', 'replaced',
                                         'concurrency', 'not_started', 'skipped',
                                         'node_lost')),
    created_at     timestamptz NOT NULL,
    CHECK ((status IN ('pending', 'running')) = (ended_at IS NULL)),
    CHECK ((status IN ('pending', 'running')) = (cause IS NULL)),
    CHECK (status <> 'running' OR started_at IS NOT NULL),
    CHECK ((cycle IS NOT NULL) = (trigger = 'manual'))
);
CREATE UNIQUE INDEX run_occurrence ON run (workload_id, scheduled_for)
    WHERE trigger = 'schedule';
CREATE UNIQUE INDEX run_cycle ON run (workload_id, cycle) WHERE trigger = 'manual';
CREATE INDEX run_by_workload ON run (workload_id, created_at DESC);
CREATE INDEX run_active ON run (workload_id) WHERE status IN ('pending', 'running');

CREATE TABLE run_state (
    workload_id     uuid PRIMARY KEY REFERENCES workload (id),
    next_due        timestamptz,
    evaluated_from  uuid REFERENCES intent (id),
    made_cycle      bigint CHECK (made_cycle >= 0)
);

GRANT SELECT, INSERT, UPDATE, DELETE ON run, run_state TO korpis_app;
