-- SPDX-License-Identifier: AGPL-3.0-or-later

DROP TRIGGER portal_certificate_announce ON certificate;
DROP FUNCTION korpis_portal_certificate_announce();
DROP TRIGGER portal_announce ON portal;
DROP FUNCTION korpis_portal_announce();
DROP TABLE portal_challenge;
