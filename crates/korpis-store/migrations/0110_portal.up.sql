-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Portals (§3.6 of docs/07-networking.md, §3.6 of docs/01-model.md): a name an organization
-- holds, under which the control plane serves its API and panel for it. A name is one portal's at
-- most, which the key holds; that it is no endpoint's route, and that its organization holds it,
-- are checked under the domains lock, which every claim of a route takes too.
--
-- A portal names the certificate it is served with, and the reference keeps that certificate from
-- being deleted while it does. An `acme` certificate made for a portal records the portal it was
-- made for, as one made for an endpoint records the endpoint, without a reference back: once the
-- portal is gone, or names another, nothing names the certificate, and the issuer's sweep removes
-- it as it removes an endpoint's.

CREATE TABLE portal (
    id               uuid PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES organization (id),
    name             text NOT NULL UNIQUE,
    certificate_id   uuid NOT NULL REFERENCES certificate (id),
    created_at       timestamptz NOT NULL
);

CREATE INDEX portal_organization ON portal (organization_id, id);
CREATE INDEX portal_certificate ON portal (certificate_id);

ALTER TABLE certificate ADD COLUMN portal_id uuid;
ALTER TABLE certificate DROP CONSTRAINT certificate_check2;
ALTER TABLE certificate ADD CONSTRAINT certificate_acme_owner CHECK (
    (source = 'acme')
        = ((workload_id IS NOT NULL AND endpoint IS NOT NULL) <> (portal_id IS NOT NULL))
);
CREATE INDEX certificate_portal ON certificate (portal_id) WHERE portal_id IS NOT NULL;

GRANT SELECT, INSERT, UPDATE, DELETE ON portal TO korpis_app;
