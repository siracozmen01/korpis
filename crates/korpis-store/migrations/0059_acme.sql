-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Certificates the control plane orders from an ACME directory (§3.2 of docs/07-networking.md).
-- An acme certificate is one endpoint's: the workload and the endpoint whose names it is for, and
-- an endpoint whose names change is given another, the one before removed once no route names it.
-- The issuer's bookkeeping is beside it: how many attempts in a row failed, when the next may be
-- made, and until when an issuer holds it, so two never order one certificate at once.
--
-- An order's HTTP-01 challenges are kept while it runs, each the token and the key authorization
-- the node's proxy answers with on port 80. Adding or removing one announces every workload whose
-- route names the certificate, as migration 0010 announces what a status is read from, so the
-- node serving the name is sent an assignment carrying it.
--
-- A new acme certificate announces itself on `korpis_certificate`, waking the issuer, which
-- otherwise looks on a fixed interval: a retry's time comes by the clock, not by announcement.

ALTER TABLE certificate
    ADD COLUMN workload_id uuid REFERENCES workload (id),
    ADD COLUMN endpoint text,
    ADD COLUMN attempts integer NOT NULL DEFAULT 0,
    ADD COLUMN next_attempt_at timestamptz,
    ADD COLUMN issuing_until timestamptz,
    ADD CHECK ((source = 'acme') = (workload_id IS NOT NULL AND endpoint IS NOT NULL));

CREATE INDEX certificate_owner ON certificate (workload_id, endpoint) WHERE source = 'acme';

CREATE TABLE certificate_challenge (
    certificate_id     uuid NOT NULL REFERENCES certificate (id) ON DELETE CASCADE,
    token              text NOT NULL,
    name               text NOT NULL,
    key_authorization  text NOT NULL,
    PRIMARY KEY (certificate_id, token)
);

CREATE FUNCTION korpis_notify_challenge() RETURNS trigger
    LANGUAGE plpgsql AS $$
DECLARE
    certificate uuid;
BEGIN
    IF TG_OP = 'DELETE' THEN
        certificate := OLD.certificate_id;
    ELSE
        certificate := NEW.certificate_id;
    END IF;
    PERFORM pg_notify('korpis_workload', r.workload_id::text)
       FROM (SELECT DISTINCT workload_id FROM route WHERE certificate_id = certificate) r;
    RETURN NULL;
END;
$$;

CREATE TRIGGER challenge_notify AFTER INSERT OR DELETE ON certificate_challenge
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_challenge();

CREATE FUNCTION korpis_certificate_announce() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('korpis_certificate', '');
    RETURN NULL;
END;
$$;

CREATE TRIGGER certificate_ordered AFTER INSERT ON certificate
    FOR EACH ROW WHEN (NEW.source = 'acme')
    EXECUTE FUNCTION korpis_certificate_announce();

GRANT SELECT, INSERT, UPDATE, DELETE ON certificate_challenge TO korpis_app;
