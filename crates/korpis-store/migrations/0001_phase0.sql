-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The Phase 0 control-plane schema (docs/03-state.md). The database enforces referential
-- integrity and immutability; it never enforces policy (§3.3). Migrations run as the owning
-- role; the control plane connects as korpis_app, which cannot update or delete what is
-- append-only (§3.1, §4).

-- Roles. Created here so that every installation has the same privilege split; the owner is
-- whoever runs the migrations.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'korpis_app') THEN
        CREATE ROLE korpis_app NOLOGIN;
    END IF;
END
$$;

-- ---------------------------------------------------------------------------------------------
-- Tenancy (§3.1 of docs/01-model.md)

CREATE TABLE organization (
    id          uuid PRIMARY KEY,
    name        text NOT NULL,
    parent_id   uuid REFERENCES organization (id),
    status      text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended')),
    suspension  jsonb,
    version     bigint NOT NULL DEFAULT 1,
    created     timestamptz NOT NULL DEFAULT now(),
    CHECK (id <> parent_id)
);
-- Unique within the parent; root organizations are unique among roots.
CREATE UNIQUE INDEX organization_name_in_parent
    ON organization (coalesce(parent_id, '00000000-0000-0000-0000-000000000000'::uuid), name);

CREATE TABLE project (
    id               uuid PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    name             text NOT NULL,
    labels           jsonb NOT NULL DEFAULT '{}',
    version          bigint NOT NULL DEFAULT 1,
    created          timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, name)
);

-- One quota entry per row so that admission can lock exactly the entries it checks and bump
-- their version in the same transaction as the intent it admits (§5.1 of
-- docs/05-scheduling.md).
CREATE TABLE quota_entry (
    id               uuid PRIMARY KEY,
    owner_kind       text NOT NULL CHECK (owner_kind IN ('organization', 'project')),
    owner_id         uuid NOT NULL,
    selector         jsonb,
    reservations     jsonb NOT NULL DEFAULT '{}',
    limits           jsonb NOT NULL DEFAULT '{}',
    egress_exhausted jsonb,
    version          bigint NOT NULL DEFAULT 1
);
CREATE INDEX quota_entry_owner ON quota_entry (owner_kind, owner_id);

-- Per-dimension overcommit an owner permits its children's limits (§5.1 of
-- docs/05-scheduling.md); absent dimensions permit none.
CREATE TABLE quota_overcommit (
    owner_kind  text NOT NULL CHECK (owner_kind IN ('organization', 'project')),
    owner_id    uuid NOT NULL,
    dimension   text NOT NULL,
    percent     integer NOT NULL CHECK (percent >= 100),
    PRIMARY KEY (owner_kind, owner_id, dimension)
);

-- ---------------------------------------------------------------------------------------------
-- Authority (§3.2 of docs/01-model.md, docs/08-identity.md)

CREATE TABLE grants (
    id              uuid PRIMARY KEY,
    -- The subject in its wire form, and a stable key for lookups by subject.
    subject         jsonb NOT NULL,
    subject_key     text NOT NULL,
    actions         text[] NOT NULL,
    scope           jsonb NOT NULL,
    scope_kind      text NOT NULL,
    scope_id        uuid NOT NULL,
    conditions      jsonb NOT NULL DEFAULT '{}',
    parent_id       uuid REFERENCES grants (id),
    delegation      jsonb NOT NULL,
    issued_by       jsonb NOT NULL,
    via             jsonb,
    from_template   jsonb,
    link_form       boolean NOT NULL DEFAULT false,
    -- Online redemption count for max_uses (§6.2 of docs/08-identity.md).
    uses            bigint NOT NULL DEFAULT 0,
    issued_at       timestamptz NOT NULL DEFAULT now(),
    revoked_at      timestamptz,
    suspended_at    timestamptz,
    -- A root grant is the only kind without a parent.
    CHECK (parent_id IS NOT NULL OR link_form = false),
    -- A link-form grant can never delegate (§3.2 of docs/08-identity.md).
    CHECK (NOT link_form OR (delegation ->> 'allowed')::boolean IS NOT TRUE)
);
CREATE INDEX grants_subject ON grants (subject_key) WHERE revoked_at IS NULL;
CREATE INDEX grants_scope ON grants (scope_kind, scope_id) WHERE revoked_at IS NULL;
CREATE INDEX grants_parent ON grants (parent_id);

-- ---------------------------------------------------------------------------------------------
-- Nodes and signing keys

CREATE TABLE node (
    id               uuid PRIMARY KEY,
    name             text NOT NULL UNIQUE,
    labels           jsonb NOT NULL DEFAULT '{}',
    taints           jsonb NOT NULL DEFAULT '[]',
    -- The node identity key's public half; the private half never leaves the node (§7 of
    -- docs/02-architecture.md).
    public_key       bytea NOT NULL UNIQUE,
    status           text NOT NULL DEFAULT 'registering' CHECK (status IN (
                         'registering', 'ready', 'cordoned', 'draining', 'unreachable',
                         'unknown', 'decommissioned')),
    capacity         jsonb NOT NULL DEFAULT '{}',
    allocatable      jsonb NOT NULL DEFAULT '{}',
    drivers          jsonb NOT NULL DEFAULT '[]',
    storage_classes  jsonb NOT NULL DEFAULT '[]',
    reachability     text NOT NULL DEFAULT 'edge_only',
    agent_version    text,
    protocol         text,
    -- Per-node heartbeat (question 6 of §10 of docs/02-architecture.md).
    last_seen        timestamptz,
    enrolled_at      timestamptz NOT NULL DEFAULT now()
);

-- Nonces a node has used in its session proof, kept for the skew window (§7 of
-- docs/02-architecture.md).
CREATE TABLE node_auth_nonce (
    node_id  uuid NOT NULL REFERENCES node (id),
    nonce    bytea NOT NULL,
    seen_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (node_id, nonce)
);

-- Public halves of the control plane's signing keys: assignment signing and capability-token
-- signing are separate purposes, and the node identity is separate from both (finding 16).
-- Historical keys stay as verify-only (§8 of docs/03-state.md).
CREATE TABLE signing_key (
    key_id      text PRIMARY KEY,
    purpose     text NOT NULL CHECK (purpose IN ('assignment', 'token')),
    public_key  bytea NOT NULL,
    not_before  timestamptz NOT NULL DEFAULT now(),
    not_after   timestamptz,
    retired_at  timestamptz
);

-- ---------------------------------------------------------------------------------------------
-- The workload and its state (§3.3 of docs/01-model.md)

CREATE TABLE workload (
    id               uuid PRIMARY KEY,
    project_id       uuid NOT NULL REFERENCES project (id),
    organization_id  uuid NOT NULL REFERENCES organization (id),
    name             text NOT NULL,
    labels           jsonb NOT NULL DEFAULT '{}',
    -- The head: the newest declared intent (§3.2 of docs/03-state.md).
    intent_id        uuid,
    intent_version   bigint NOT NULL DEFAULT 0,
    -- The target: the approved intent nodes converge to; moved only by approval.
    target_id        uuid,
    target_version   bigint NOT NULL DEFAULT 0,
    -- The current power declaration (§3.3 of docs/01-model.md).
    power_version    bigint NOT NULL DEFAULT 0,
    -- The highest version ever issued, advanced past recovered versions after a restore so no
    -- number is issued twice (§8 of docs/03-state.md).
    version_floor    bigint NOT NULL DEFAULT 0,
    -- Metadata (name, labels) version for compare-and-swap.
    version          bigint NOT NULL DEFAULT 1,
    created          timestamptz NOT NULL DEFAULT now(),
    deleted_at       timestamptz
);
CREATE UNIQUE INDEX workload_name_in_project ON workload (project_id, name)
    WHERE deleted_at IS NULL;
CREATE INDEX workload_organization ON workload (organization_id);

CREATE TABLE intent (
    id              uuid PRIMARY KEY,
    workload_id     uuid NOT NULL REFERENCES workload (id),
    version         bigint NOT NULL CHECK (version > 0),
    parent_id       uuid REFERENCES intent (id),
    declared_by     jsonb NOT NULL,
    declared_at     timestamptz NOT NULL DEFAULT now(),
    -- The protobuf IntentBody in canonical ProtoJSON, defaults emitted (§3.1).
    body            jsonb NOT NULL,
    -- The schema in force when the row was written.
    schema_version  text NOT NULL,
    -- Recovered from a node's signed assignment after a restore (§8).
    recovered_from  jsonb,
    UNIQUE (workload_id, version)
);
CREATE INDEX intent_recipe_digest ON intent ((body -> 'recipe' ->> 'digest'));

ALTER TABLE workload
    ADD CONSTRAINT workload_intent_fk FOREIGN KEY (intent_id) REFERENCES intent (id),
    ADD CONSTRAINT workload_target_fk FOREIGN KEY (target_id) REFERENCES intent (id);

-- Power declarations: versioned, attributed, append-only like intents.
CREATE TABLE power (
    workload_id  uuid NOT NULL REFERENCES workload (id),
    version      bigint NOT NULL CHECK (version > 0),
    desired      text NOT NULL CHECK (desired IN ('running', 'stopped')),
    cycle        bigint NOT NULL DEFAULT 0,
    set_by       jsonb NOT NULL,
    set_at       timestamptz NOT NULL DEFAULT now(),
    recovered_from jsonb,
    PRIMARY KEY (workload_id, version)
);

CREATE TABLE plan (
    id              uuid PRIMARY KEY,
    workload_id     uuid NOT NULL REFERENCES workload (id),
    kind            text NOT NULL CHECK (kind IN ('intent', 'power', 'retire', 'migrate')),
    operation_id    uuid,
    from_intent     uuid REFERENCES intent (id),
    to_intent       uuid REFERENCES intent (id),
    to_power        bigint,
    computed_at     timestamptz NOT NULL DEFAULT now(),
    computed_by     jsonb NOT NULL,
    steps           jsonb NOT NULL,
    impact          jsonb NOT NULL,
    status          text NOT NULL CHECK (status IN (
                        'proposed', 'approved', 'queued', 'applying', 'applied', 'failed',
                        'cancelled', 'rejected', 'expired', 'superseded')),
    completed       integer,
    -- The last step index the executing nodes may run; lowering it cancels (§7).
    through         integer,
    apply_after     timestamptz,
    expires_at      timestamptz NOT NULL,
    approved_by     jsonb,
    approved_at     timestamptz,
    proposed_under  uuid REFERENCES grants (id),
    recovered       boolean NOT NULL DEFAULT false,
    CHECK ((kind = 'intent') = (to_intent IS NOT NULL)),
    CHECK ((kind = 'power') = (to_power IS NOT NULL))
);
-- Plans are serialized per workload: at most one applying (§7 of docs/03-state.md).
CREATE UNIQUE INDEX plan_one_applying ON plan (workload_id) WHERE status = 'applying';
CREATE INDEX plan_workload ON plan (workload_id, computed_at DESC);

-- ---------------------------------------------------------------------------------------------
-- Placement, leases, assignments

CREATE TABLE placement (
    workload_id    uuid NOT NULL REFERENCES workload (id),
    node_id        uuid NOT NULL REFERENCES node (id),
    bound_at       timestamptz NOT NULL DEFAULT now(),
    bound_by       text NOT NULL CHECK (bound_by IN ('scheduler', 'operator', 'migration')),
    reason         text NOT NULL,
    explanation    jsonb NOT NULL,
    authoritative  boolean NOT NULL DEFAULT true,
    PRIMARY KEY (workload_id, node_id)
);
-- A persistent workload has at most one authoritative placement.
CREATE UNIQUE INDEX placement_one_authoritative ON placement (workload_id)
    WHERE authoritative;

-- Exactly one valid lease per workload; the epoch increments on every issue and is never
-- reused (§4.5 of docs/02-architecture.md).
CREATE TABLE lease (
    workload_id     uuid PRIMARY KEY REFERENCES workload (id),
    node_id         uuid NOT NULL REFERENCES node (id),
    epoch           bigint NOT NULL CHECK (epoch > 0),
    term_ms         bigint NOT NULL CHECK (term_ms > 0),
    -- The grantor's end of the lease: received_at + term, on the control plane's clock.
    received_at     timestamptz NOT NULL,
    grantor_expiry  timestamptz NOT NULL,
    released_at     timestamptz
);

-- The epoch floor and the issue-rate record behind the restore fence (§8 of
-- docs/03-state.md). One row.
CREATE TABLE epoch_state (
    singleton            boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    max_issued_epoch     bigint NOT NULL DEFAULT 0,
    floor                bigint NOT NULL DEFAULT 0,
    issue_rate_per_sec   bigint NOT NULL DEFAULT 1000,
    settling_until       timestamptz
);
INSERT INTO epoch_state DEFAULT VALUES;

-- The signed assignment currently published for each workload on each node (§4.8 of
-- docs/02-architecture.md), from which the set digest is computed.
CREATE TABLE assignment (
    workload_id  uuid NOT NULL REFERENCES workload (id),
    node_id      uuid NOT NULL REFERENCES node (id),
    generation   bigint NOT NULL CHECK (generation > 0),
    body         bytea NOT NULL,
    signature    bytea NOT NULL,
    key_id       text NOT NULL REFERENCES signing_key (key_id),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (workload_id, node_id)
);
CREATE INDEX assignment_node ON assignment (node_id);

-- The latest observation per workload, updated in place: "now", not history (§5).
CREATE TABLE observation_latest (
    workload_id  uuid PRIMARY KEY REFERENCES workload (id),
    node_id      uuid NOT NULL REFERENCES node (id),
    observed_at  timestamptz NOT NULL,
    received_at  timestamptz NOT NULL DEFAULT now(),
    state        text NOT NULL,
    body         jsonb NOT NULL
);

-- ---------------------------------------------------------------------------------------------
-- The effect log (§4 of docs/03-state.md)

CREATE TABLE effect (
    id           uuid NOT NULL,
    -- The ingest clock: the only ordering clock, the primary key and the partition key.
    at           timestamptz NOT NULL DEFAULT clock_timestamp(),
    -- The originating node's reading, never a key.
    occurred_at  timestamptz,
    plan_id      uuid,
    step         integer,
    workload_id  uuid,
    node_id      uuid,
    actor        jsonb NOT NULL,
    grant_id     uuid,
    action       text NOT NULL,
    outcome      text NOT NULL CHECK (outcome IN (
                     'succeeded', 'failed', 'skipped', 'rolled_back', 'denied')),
    error        jsonb,
    before       jsonb,
    after        jsonb,
    -- Coalesced repeats (§10 of docs/10-api.md).
    count        bigint NOT NULL DEFAULT 1,
    window_end   timestamptz,
    origin_node  uuid,
    origin_seq   bigint,
    PRIMARY KEY (at, id),
    CHECK ((origin_node IS NULL) = (origin_seq IS NULL))
) PARTITION BY RANGE (at);
CREATE INDEX effect_workload ON effect (workload_id, at);
CREATE INDEX effect_plan ON effect (plan_id, step);

-- Monthly partitions, created ahead of need. Called by the migration and by the control plane
-- at start and daily; detaching an old partition is how effects are archived, never deleted.
CREATE FUNCTION korpis_ensure_effect_partitions(months_ahead integer) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public AS $$
DECLARE
    month_start date := date_trunc('month', now() - interval '1 month')::date;
    i integer;
    part text;
BEGIN
    FOR i IN 0 .. months_ahead + 1 LOOP
        part := format('effect_%s', to_char(month_start + make_interval(months => i), 'YYYY_MM'));
        IF to_regclass(part) IS NULL THEN
            EXECUTE format(
                'CREATE TABLE %I PARTITION OF effect FOR VALUES FROM (%L) TO (%L)',
                part,
                month_start + make_interval(months => i),
                month_start + make_interval(months => i + 1));
            EXECUTE format('REVOKE UPDATE, DELETE, TRUNCATE ON %I FROM korpis_app', part);
            EXECUTE format('GRANT SELECT, INSERT ON %I TO korpis_app', part);
        END IF;
    END LOOP;
END
$$;
SELECT korpis_ensure_effect_partitions(3);

-- Exactly-once, in-order ingestion of node journals (§4.10 of docs/02-architecture.md).
CREATE TABLE node_journal_cursor (
    node_id           uuid PRIMARY KEY REFERENCES node (id),
    ingested_through  bigint NOT NULL DEFAULT 0
);

-- ---------------------------------------------------------------------------------------------
-- Job queue (§6 of docs/03-state.md)

CREATE TABLE job (
    id            uuid PRIMARY KEY,
    -- (plan_id, step_index) for plan steps: enqueueing twice is a no-op.
    key           text NOT NULL UNIQUE,
    kind          text NOT NULL,
    payload       jsonb NOT NULL,
    run_after     timestamptz NOT NULL DEFAULT now(),
    attempts      integer NOT NULL DEFAULT 0,
    max_attempts  integer NOT NULL DEFAULT 10,
    locked_by     text,
    locked_until  timestamptz,
    status        text NOT NULL DEFAULT 'pending' CHECK (status IN (
                      'pending', 'running', 'succeeded', 'failed', 'cancelled')),
    last_error    jsonb,
    created       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX job_ready ON job (run_after) WHERE status = 'pending';

-- API idempotency keys (§6 of docs/10-api.md): a repeat returns the original outcome.
CREATE TABLE idempotency (
    subject_key  text NOT NULL,
    key          text NOT NULL,
    method       text NOT NULL,
    response     bytea NOT NULL,
    created      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (subject_key, key)
);

-- ---------------------------------------------------------------------------------------------
-- Privileges

GRANT USAGE ON SCHEMA public TO korpis_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON
    organization, project, quota_entry, quota_overcommit, grants, node, node_auth_nonce,
    signing_key, workload, plan, placement, lease, epoch_state, assignment, observation_latest,
    node_journal_cursor, job, idempotency
    TO korpis_app;
-- Append-only by constraint, not by convention (§3.1, §4).
GRANT SELECT, INSERT ON intent, power, effect TO korpis_app;
REVOKE UPDATE, DELETE, TRUNCATE ON intent, power, effect FROM korpis_app;
GRANT EXECUTE ON FUNCTION korpis_ensure_effect_partitions(integer) TO korpis_app;
