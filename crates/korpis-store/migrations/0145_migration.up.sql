-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Migrations (§8 and §8.1 of docs/05-scheduling.md). A migration carries out one approved plan
-- of kind `migrate`, moving its workload from the source node to the destination through seven
-- phases, each advanced by the control plane on both nodes' reports. The destination is a second
-- placement of the workload, not authoritative until cutover. Each volume the workload's target
-- attaches crosses as the classes on both nodes allow: by increments, by its tree, or not at all;
-- its row keeps the passes sent and what each side's manifest digest was. Each stream's
-- watermark, its next offset and the position its changes log was journaled to on the source, is
-- kept from the source's report at quiesce and handed to the destination at cutover. A workload
-- has one migration at a time; one that ended is kept, with how and why.

CREATE TABLE migration (
    id               uuid PRIMARY KEY,
    plan_id          uuid NOT NULL UNIQUE REFERENCES plan (id),
    workload_id      uuid NOT NULL REFERENCES workload (id),
    source_id        uuid NOT NULL REFERENCES node (id),
    destination_id   uuid NOT NULL REFERENCES node (id),
    phase            text NOT NULL CHECK (phase IN ('prepare', 'replicate', 'quiesce',
                                                    'final_sync', 'verify', 'cutover',
                                                    'release')),
    phase_since      timestamptz NOT NULL,
    hold_secs        bigint NOT NULL CHECK (hold_secs > 0),
    hold_until       timestamptz,
    outcome          text CHECK (outcome IN ('completed', 'rolled_back', 'abandoned')),
    reason           text,
    made_at          timestamptz NOT NULL,
    ended_at         timestamptz,
    CONSTRAINT migration_two_nodes CHECK (source_id <> destination_id),
    CONSTRAINT migration_ended CHECK (
        (outcome IS NULL) = (ended_at IS NULL) AND (outcome IS NULL) = (reason IS NULL))
);

CREATE UNIQUE INDEX migration_live ON migration (workload_id) WHERE outcome IS NULL;
CREATE INDEX migration_workload ON migration (workload_id, made_at);

CREATE TABLE migration_volume (
    migration_id     uuid NOT NULL REFERENCES migration (id),
    volume_id        uuid NOT NULL REFERENCES volume (id),
    crossing         text NOT NULL CHECK (crossing IN ('increments', 'tree', 'nothing')),
    size_bytes       bigint NOT NULL CHECK (size_bytes >= 0),
    source_helper_id text NOT NULL,
    passes           integer NOT NULL DEFAULT 0 CHECK (passes >= 0),
    pass             uuid,
    pass_since       timestamptz,
    sent_bytes       bigint CHECK (sent_bytes >= 0),
    held_pass        uuid,
    destination_helper_id text,
    source_manifest  text,
    destination_manifest text,
    first_difference text,
    PRIMARY KEY (migration_id, volume_id)
);

CREATE TABLE migration_stream (
    migration_id     uuid NOT NULL REFERENCES migration (id),
    stream           text NOT NULL,
    next_offset      bigint NOT NULL CHECK (next_offset >= 0),
    journaled_to     bigint NOT NULL CHECK (journaled_to >= 0),
    PRIMARY KEY (migration_id, stream)
);

GRANT SELECT, INSERT, UPDATE ON migration, migration_volume, migration_stream TO korpis_app;

-- A phase moved or a migration ended changes what both nodes' assignments carry.
CREATE TRIGGER migration_notify AFTER INSERT OR UPDATE ON migration
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_workload();
