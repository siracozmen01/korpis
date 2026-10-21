-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Serving portals (§3.6 of docs/07-networking.md). A portal's TLS-ALPN-01 challenge is the digest
-- the directory asks for, by the name it is for, recorded while its order runs: whichever replica
-- the directory's connection reaches reads it for that handshake, so the replicas need not agree
-- on anything first. A row goes with its certificate's order, and with the certificate.
--
-- Every replica holds the portals and their certificates in memory to choose a connection's
-- certificate by its SNI, and is told to read them again on `korpis_portal` whenever a portal is
-- made, changed, or deleted, or the certificate one is served with changes; it also reads them
-- every minute, an announcement being a wake-up and not a record.

CREATE TABLE portal_challenge (
    name            text PRIMARY KEY,
    certificate_id  uuid NOT NULL REFERENCES certificate (id) ON DELETE CASCADE,
    digest          bytea NOT NULL CHECK (length(digest) = 32)
);
GRANT SELECT, INSERT, UPDATE, DELETE ON portal_challenge TO korpis_app;

CREATE FUNCTION korpis_portal_announce() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('korpis_portal', '');
    RETURN NULL;
END;
$$;

CREATE TRIGGER portal_announce AFTER INSERT OR UPDATE OR DELETE ON portal
    FOR EACH STATEMENT EXECUTE FUNCTION korpis_portal_announce();

CREATE FUNCTION korpis_portal_certificate_announce() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM portal WHERE certificate_id = NEW.id) THEN
        PERFORM pg_notify('korpis_portal', '');
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER portal_certificate_announce AFTER UPDATE OF revision ON certificate
    FOR EACH ROW EXECUTE FUNCTION korpis_portal_certificate_announce();
