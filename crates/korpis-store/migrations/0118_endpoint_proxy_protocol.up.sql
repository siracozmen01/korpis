-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Whether an ingress endpoint whose TLS is passed through declared `client_address:
-- proxy_protocol`, so the proxy writes its workload a PROXY header, version 2, naming the client
-- before the first byte it splices (§3.2 of docs/07-networking.md). Its node's assignment
-- carries it to the proxy; on any other endpoint it is refused at declaration, and here.

ALTER TABLE endpoint
    ADD COLUMN proxy_protocol boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT endpoint_proxy_protocol_passthrough
        CHECK (NOT proxy_protocol OR tls = 'passthrough');
