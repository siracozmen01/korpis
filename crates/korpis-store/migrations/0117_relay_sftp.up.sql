-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Where each relay serving SFTP is reached, and the host key it serves, as the relay last
-- reported them in its signed feed request (§3.1 of docs/02-architecture.md, §3 of
-- docs/18-operations.md), so ServerInfo can tell a client where to connect and which key to
-- expect. A relay that stops serving SFTP reports nothing, and its row goes.

CREATE TABLE relay_sftp (
    relay_id     uuid PRIMARY KEY REFERENCES relay (id),
    address      text NOT NULL CHECK (char_length(address) BETWEEN 3 AND 300),
    host_key     bytea NOT NULL CHECK (octet_length(host_key) BETWEEN 1 AND 4096),
    reported_at  timestamptz NOT NULL
);

GRANT SELECT, INSERT, UPDATE, DELETE ON relay_sftp TO korpis_app;
