-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The client certificates an ingress endpoint whose TLS the proxy terminates asks for, and the
-- certificates, in DER, of the authorities it verifies them against (§3.2 of
-- docs/07-networking.md). Its node's assignment carries them to the proxy. They are asked for
-- only where the proxy terminates TLS, and authorities are held exactly where they are asked for.

ALTER TABLE endpoint
    ADD COLUMN client_certificates text
        CHECK (client_certificates IN ('required', 'optional')),
    ADD COLUMN client_ca bytea[] NOT NULL DEFAULT '{}',
    ADD CONSTRAINT endpoint_client_certificates_terminated
        CHECK (client_certificates IS NULL OR tls IN ('acme', 'provided')),
    ADD CONSTRAINT endpoint_client_ca_asked
        CHECK ((client_certificates IS NULL) = (cardinality(client_ca) = 0));
