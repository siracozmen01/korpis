-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Which certificate an ingress endpoint is served with, and at which revision (§3.2 of
-- docs/07-networking.md). The endpoint as placed records both, so its node's assignment carries
-- them: a certificate replaced or renewed raises its revision, which changes the assignment of
-- every endpoint serving with it, which its node is sent and fetches the certificate again for.
-- No reference: the record says what was served, and a certificate is kept from deletion by the
-- routes that name it, not by this.
--
-- A certificate's new revision announces every workload whose route names it, as migration 0010
-- announces every change a workload's status is read from, so no writer can forget it.

ALTER TABLE endpoint
    ADD COLUMN certificate_id uuid,
    ADD COLUMN certificate_revision bigint,
    ADD CHECK ((certificate_id IS NULL) = (certificate_revision IS NULL));

CREATE FUNCTION korpis_notify_certificate() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('korpis_workload', r.workload_id::text)
       FROM (SELECT DISTINCT workload_id FROM route WHERE certificate_id = NEW.id) r;
    RETURN NULL;
END;
$$;

CREATE TRIGGER certificate_notify AFTER UPDATE ON certificate
    FOR EACH ROW WHEN (OLD.revision IS DISTINCT FROM NEW.revision)
    EXECUTE FUNCTION korpis_notify_certificate();
