-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Binds a shared store to the declaring node's organization where no organization holds it:
-- none declared it before, or the one that did keeps no live volume in it and no node of its own
-- offering it (§2.1 of docs/06-storage.md, Finding 203 of docs/23-walkthroughs.md). A node of an
-- organization refused the store never recorded it among its classes, so it holds nothing here.
INSERT INTO shared_store (id, organization_id, first_node, bound_at)
VALUES ($1, $2, $3, $4)
ON CONFLICT (id) DO UPDATE
    SET organization_id = EXCLUDED.organization_id,
        first_node = EXCLUDED.first_node,
        bound_at = EXCLUDED.bound_at
    WHERE shared_store.organization_id <> EXCLUDED.organization_id
      AND NOT EXISTS (
          SELECT 1 FROM volume v WHERE v.store = shared_store.id AND v.deleted_at IS NULL)
      AND NOT EXISTS (
          SELECT 1 FROM node n
          WHERE n.organization_id = shared_store.organization_id
            AND n.admin <> 'decommissioned'
            AND n.storage_classes @> jsonb_build_array(jsonb_build_object('store', shared_store.id)))
