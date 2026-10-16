-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The DNS-01 challenge records an order of an acme certificate published through a DNS provider
-- (§3.2 of docs/07-networking.md): the provider, by name as the certificate's organization sees
-- it, and the owner the record was applied for, one per name of the certificate. A row is written
-- before the provider is asked and removed once the record is withdrawn, so a record published by
-- an issuer that stopped mid-order is still known, and withdrawn by the next issuer that looks.
-- A certificate with a record standing is not removed until it is withdrawn.

CREATE TABLE certificate_record (
    certificate_id  uuid NOT NULL REFERENCES certificate (id),
    provider        text NOT NULL,
    owner           text NOT NULL,
    PRIMARY KEY (certificate_id, provider, owner)
);

GRANT SELECT, INSERT, DELETE ON certificate_record TO korpis_app;
