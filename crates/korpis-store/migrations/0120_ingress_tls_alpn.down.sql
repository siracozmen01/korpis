-- SPDX-License-Identifier: AGPL-3.0-or-later

ALTER TABLE certificate DROP COLUMN node_proof;
DROP TABLE certificate_alpn_challenge;
