-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A shared store is bound again to the organization of a node declaring it once the organization
-- holding it keeps no live volume in it and no node offering it (Finding 203 of
-- docs/23-walkthroughs.md), so a store an operator moves to another organization's nodes is not
-- held forever by the first.

GRANT UPDATE ON shared_store TO korpis_app;
