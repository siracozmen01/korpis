-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- How close a relay's certificate came to expiring, as last reported by an event (§7 of
-- docs/18-operations.md): each step is reported once, and a renewal clears it.

ALTER TABLE relay
    ADD COLUMN certificate_alert text CHECK (certificate_alert IN ('warning', 'critical'));
