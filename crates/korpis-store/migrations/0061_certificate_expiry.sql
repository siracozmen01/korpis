-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The expiry alert last reported for a certificate (§3.2 of docs/07-networking.md, §7 of
-- docs/18-operations.md): `warning` or `critical`, as `certificate.expiring` events say, each
-- reported once for one validity period. A chain whose validity differs, a renewal or a
-- replacement, starts the count again.

ALTER TABLE certificate
    ADD COLUMN expiry_alert text CHECK (expiry_alert IN ('warning', 'critical'));
