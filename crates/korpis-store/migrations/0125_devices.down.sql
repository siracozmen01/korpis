-- SPDX-License-Identifier: AGPL-3.0-or-later

ALTER TABLE placement DROP COLUMN devices;
ALTER TABLE node DROP COLUMN devices, DROP COLUMN unoffered_devices;
