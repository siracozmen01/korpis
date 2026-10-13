-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Workload credentials (§9.5 of docs/08-identity.md): issued through the node that runs a
-- workload, for fifteen minutes, stored only as the SHA-256 digest of the secret, with the node
-- and lease epoch it was issued under. A credential authenticates only while its workload is
-- still placed, authoritatively, on that node.

CREATE TABLE workload_credential (
    secret_hash  bytea PRIMARY KEY,
    workload_id  uuid NOT NULL REFERENCES workload (id),
    -- The principal key it authenticates as: workload:<id> or extension:<id>.
    subject      text NOT NULL,
    node_id      uuid NOT NULL REFERENCES node (id),
    epoch        bigint NOT NULL CHECK (epoch > 0),
    issued_at    timestamptz NOT NULL,
    expires_at   timestamptz NOT NULL CHECK (expires_at > issued_at)
);
CREATE INDEX workload_credential_expiry ON workload_credential (expires_at);

GRANT SELECT, INSERT, DELETE ON workload_credential TO korpis_app;
