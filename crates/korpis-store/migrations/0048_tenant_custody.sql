-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Tenant key custody (§5.5 of docs/06-storage.md): a repository whose data keys are made and
-- opened by the tenant's own key management service, through a key_management provider, names
-- that provider and the tenant's key, and has no key-encryption key here. Its workload keys are
-- stored as the tenant's service wrapped them, and open only through that service.

ALTER TABLE repository ADD COLUMN kms_provider text;
ALTER TABLE repository ADD COLUMN kms_key text CHECK (length(kms_key) BETWEEN 1 AND 1024);
ALTER TABLE repository ALTER COLUMN kek_sealed DROP NOT NULL;
ALTER TABLE repository ALTER COLUMN kek_sealed_by DROP NOT NULL;
ALTER TABLE repository ADD CONSTRAINT repository_custody CHECK (
    CASE key_custody
        WHEN 'operator' THEN kek_sealed IS NOT NULL AND kek_sealed_by IS NOT NULL
                             AND kms_provider IS NULL AND kms_key IS NULL
        ELSE kek_sealed IS NULL AND kek_sealed_by IS NULL
             AND kms_provider IS NOT NULL AND kms_key IS NOT NULL
    END);
