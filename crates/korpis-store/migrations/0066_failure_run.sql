-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Runs of identical failed attempts to authenticate being coalesced (§10 of docs/10-api.md), as
-- denial_run holds denials: the first failure of a source, kind, and target is in the effect log
-- at once, and its repeats within the window are counted here and enter the log as one effect,
-- with the count and the window's bounds, when the window closes. The source is the address, or
-- the /64 of an IPv6 one; the target is what the attempt named, `<kind>:<id>`, or empty. This is
-- working state, not the log.

CREATE TABLE failure_run (
    source        text NOT NULL,
    action        text NOT NULL,
    target        text NOT NULL,
    opened        timestamptz NOT NULL,
    repeats       bigint NOT NULL CHECK (repeats >= 0),
    first_repeat  timestamptz,
    last_repeat   timestamptz,
    PRIMARY KEY (source, action, target),
    CHECK ((repeats = 0) = (first_repeat IS NULL)),
    CHECK ((first_repeat IS NULL) = (last_repeat IS NULL))
);
CREATE INDEX failure_run_opened ON failure_run (opened);

GRANT SELECT, INSERT, UPDATE, DELETE ON failure_run TO korpis_app;
