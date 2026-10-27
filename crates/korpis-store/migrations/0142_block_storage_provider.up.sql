-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Block storage providers (§2.1 of docs/06-storage.md, §5 of docs/16-extensions.md): each says,
-- when it registers, whether its attach refuses every other node in the storage itself, which the
-- control plane takes as its class's; every other provider says nothing of it.

ALTER TABLE provider_registration ADD COLUMN fencing text NOT NULL DEFAULT 'none'
    CHECK (fencing IN ('none', 'lease_only', 'storage_enforced'));
