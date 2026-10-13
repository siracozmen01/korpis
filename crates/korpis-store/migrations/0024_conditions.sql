-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Conditions as events (§5 of docs/15-observability.md): a workload's health and convergence
-- changing are events of their own, written where the control plane decides them. Every event,
-- an effect's or a condition's, is appended through one function, so every one takes its key's
-- counter row the same way.

-- Appends an event under `k`, numbered by the key's counter row.
CREATE FUNCTION korpis_append_event(
    k text, at timestamptz, kind text, severity text, org uuid, workload uuid, effect uuid,
    detail jsonb
) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    n bigint;
BEGIN
    INSERT INTO event_key (key, seq) VALUES (k, 1)
        ON CONFLICT (key) DO UPDATE SET seq = event_key.seq + 1
        RETURNING seq INTO n;
    INSERT INTO event (key, seq, at, kind, severity, organization_id, workload_id, effect_id,
                       detail)
    VALUES (k, n, at, kind, severity, org, workload, effect, detail);
    PERFORM pg_notify('korpis_event_written', '');
END
$$;

CREATE OR REPLACE FUNCTION korpis_effect_event() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    org uuid;
BEGIN
    org := coalesce(
        (SELECT organization_id FROM workload WHERE id = NEW.workload_id),
        (SELECT organization_id FROM node WHERE id = NEW.node_id),
        (SELECT korpis_object_organization(scope_kind, scope_id) FROM grants
          WHERE id = NEW.grant_id));
    IF org IS NULL THEN
        RETURN NULL;
    END IF;
    PERFORM korpis_append_event(
        CASE WHEN NEW.workload_id IS NOT NULL THEN 'workload:' || NEW.workload_id
             ELSE 'organization:' || org END,
        NEW.at, NEW.action,
        CASE WHEN NEW.outcome IN ('succeeded', 'skipped') THEN 'info' ELSE 'warning' END,
        org, NEW.workload_id, NEW.id,
        coalesce(NEW.after, '{}'::jsonb) || coalesce(NEW.error, '{}'::jsonb));
    RETURN NULL;
END
$$;

-- A workload's convergence as last decided, so a change is written once, by whichever control
-- plane process decides it first.
CREATE TABLE workload_condition (
    workload_id  uuid PRIMARY KEY REFERENCES workload (id),
    convergence  text NOT NULL,
    at           timestamptz NOT NULL
);

GRANT SELECT, INSERT, UPDATE, DELETE ON workload_condition TO korpis_app;
