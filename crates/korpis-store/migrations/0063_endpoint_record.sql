-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- An ingress endpoint's names in DNS (§3.5 of docs/07-networking.md): one row per name of an
-- endpoint the ingress serves, with the addresses it should resolve to (its node's advertised
-- addresses; NULL once nothing routes it, to be withdrawn), what the provider last made it, and
-- through which provider. The owner the records are applied for is the endpoint's public
-- identifier and the first sixteen hex digits of the name's SHA-256, `ep_…/…`: one owner per
-- name, so each name is published through whichever provider serves its zone, and within the
-- 128 bytes an owner may take. The row outlives its endpoint until the name is withdrawn, so it
-- names the endpoint without a reference. A reconciler takes a row due under a lease,
-- `applying_until`, as the issuer takes a certificate.
--
-- An endpoint placed, moved, or removed, or a node's advertised addresses changing, announces
-- itself on `korpis_endpoint`, waking the reconciler; a write that changes none of them does not.

CREATE TABLE endpoint_record (
    endpoint_id      uuid NOT NULL,
    name             text NOT NULL,
    workload_id      uuid NOT NULL REFERENCES workload (id),
    organization_id  uuid NOT NULL REFERENCES organization (id),
    owner            text NOT NULL,
    wanted           text[],
    applied          text[],
    provider         text,
    state            text NOT NULL
                     CHECK (state IN ('pending', 'published', 'unmanaged', 'failed')),
    reason           text,
    attempts         integer NOT NULL DEFAULT 0,
    next_attempt_at  timestamptz,
    applying_until   timestamptz,
    PRIMARY KEY (endpoint_id, name)
);

CREATE INDEX endpoint_record_workload ON endpoint_record (workload_id);

CREATE FUNCTION korpis_endpoint_announce() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('korpis_endpoint', '');
    RETURN NULL;
END;
$$;

CREATE TRIGGER endpoint_placed AFTER INSERT OR DELETE ON endpoint
    FOR EACH ROW EXECUTE FUNCTION korpis_endpoint_announce();

CREATE TRIGGER endpoint_moved AFTER UPDATE ON endpoint
    FOR EACH ROW
    WHEN (OLD.node_id IS DISTINCT FROM NEW.node_id
          OR OLD.domains IS DISTINCT FROM NEW.domains
          OR OLD.exposure IS DISTINCT FROM NEW.exposure)
    EXECUTE FUNCTION korpis_endpoint_announce();

CREATE TRIGGER node_readdressed AFTER UPDATE OF addresses ON node
    FOR EACH ROW WHEN (OLD.addresses IS DISTINCT FROM NEW.addresses)
    EXECUTE FUNCTION korpis_endpoint_announce();

GRANT SELECT, INSERT, UPDATE, DELETE ON endpoint_record TO korpis_app;
