-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- ACME's TLS-ALPN-01 challenge at a node's ingress (§3.2 of docs/07-networking.md). While an
-- order runs, each name a node's proxy proves on port 443 has its digest here, carried to the
-- nodes serving the certificate's routes in their assignments, as the HTTP-01 tokens are, and
-- announced to them the same way. A certificate keeps the way a node's proxy last proved it, so
-- its renewals try that way first.

CREATE TABLE certificate_alpn_challenge (
    certificate_id  uuid NOT NULL REFERENCES certificate (id) ON DELETE CASCADE,
    name            text NOT NULL,
    digest          bytea NOT NULL CHECK (octet_length(digest) = 32),
    PRIMARY KEY (certificate_id, name)
);

CREATE TRIGGER alpn_challenge_notify AFTER INSERT OR DELETE ON certificate_alpn_challenge
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_challenge();

GRANT SELECT, INSERT, UPDATE, DELETE ON certificate_alpn_challenge TO korpis_app;

ALTER TABLE certificate
    ADD COLUMN node_proof text CHECK (node_proof IN ('http-01', 'tls-alpn-01'));
