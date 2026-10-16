-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Which nodes were given which certificate keys, so removing a node as compromised revokes and
-- replaces what it held (§9 of docs/17-security.md). Each certificate records its key's
-- identifier, the SHA-256 of its leaf's subject public key info, and whether that key was given
-- to a node later removed as compromised; a chain for another key clears it.
--
-- An exposure is written when a node is answered a certificate and its key: the revision, the
-- key, and the chain it was given, kept because a renewal replaces the certificate's own chain
-- and an exposed chain is revoked by presenting it. Once the node is removed as compromised, each
-- exposed chain of an acme certificate is revoked through the directory, and the outcome kept.

ALTER TABLE certificate
    ADD COLUMN key_id text,
    ADD COLUMN exposed boolean NOT NULL DEFAULT false;

ALTER TABLE node ADD COLUMN compromised_at timestamptz;

CREATE TABLE certificate_exposure (
    certificate_id  uuid NOT NULL REFERENCES certificate (id) ON DELETE CASCADE,
    node_id         uuid NOT NULL REFERENCES node (id),
    revision        bigint NOT NULL,
    key_id          text NOT NULL,
    chain           text NOT NULL,
    given_at        timestamptz NOT NULL,
    revoked_at      timestamptz,
    revocation      text,
    PRIMARY KEY (certificate_id, node_id, revision)
);

CREATE INDEX certificate_exposure_node ON certificate_exposure (node_id);

GRANT SELECT, INSERT, UPDATE ON certificate_exposure TO korpis_app;
