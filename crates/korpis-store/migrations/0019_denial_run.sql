-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Runs of identical denials being coalesced (§10 of docs/10-api.md, §4 of
-- docs/15-observability.md). The first denial of a subject, action, and object is in the effect
-- log at once; its repeats within the window are counted here and enter the log as one effect,
-- with the count and the window's bounds, when the window closes. This is working state, not the
-- log: a run's row is replaced as it counts and deleted once it is written.

CREATE TABLE denial_run (
    actor         text NOT NULL,
    action        text NOT NULL,
    object_kind   text NOT NULL,
    object_id     uuid NOT NULL,
    opened        timestamptz NOT NULL,
    repeats       bigint NOT NULL CHECK (repeats >= 0),
    first_repeat  timestamptz,
    last_repeat   timestamptz,
    PRIMARY KEY (actor, action, object_kind, object_id),
    CHECK ((repeats = 0) = (first_repeat IS NULL)),
    CHECK ((first_repeat IS NULL) = (last_repeat IS NULL))
);
CREATE INDEX denial_run_opened ON denial_run (opened);

GRANT SELECT, INSERT, UPDATE, DELETE ON denial_run TO korpis_app;
