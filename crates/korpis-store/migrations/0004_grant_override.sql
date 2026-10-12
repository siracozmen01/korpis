-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A set-subject grant may carry irreversible actions only where the operator overrode the
-- default on its identity binding (§5.2 of docs/08-identity.md). The control plane copies that
-- decision onto the grant at issue; a link-form grant never carries it.

ALTER TABLE grants ADD COLUMN irreversible_override boolean NOT NULL DEFAULT false;
ALTER TABLE grants ADD CONSTRAINT grants_link_never_overridden
    CHECK (NOT (link_form AND irreversible_override));
