-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The ingress on one machine (§3.2 of docs/07-networking.md). A node says whether it holds ports
-- 80 and 443 of its addresses for the proxy, which placement requires of a workload with an
-- ingress endpoint. An endpoint as placed keeps the names it routes and where its certificate
-- comes from, which its node's assignment carries to the proxy.

ALTER TABLE node ADD COLUMN ingress boolean NOT NULL DEFAULT false;

ALTER TABLE endpoint
    ADD COLUMN domains text[] NOT NULL DEFAULT '{}',
    ADD COLUMN tls text CHECK (tls IN ('acme', 'provided', 'passthrough')),
    -- For tls = 'provided': the certificate, by identifier.
    ADD COLUMN tls_secret text NOT NULL DEFAULT '';
