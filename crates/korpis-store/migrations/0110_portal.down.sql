-- SPDX-License-Identifier: AGPL-3.0-or-later

DROP TABLE portal;
DELETE FROM certificate WHERE portal_id IS NOT NULL;
ALTER TABLE certificate DROP CONSTRAINT certificate_acme_owner;
ALTER TABLE certificate DROP COLUMN portal_id;
ALTER TABLE certificate
    ADD CHECK ((source = 'acme') = (workload_id IS NOT NULL AND endpoint IS NOT NULL));
