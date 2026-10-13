-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The operator's overrides of how a workload's plan steps are classified (§3.3 of
-- docs/01-model.md): the last layer of the chain, each entry an action and whether it is
-- disruptive. Set as a whole under the workload's version; every change is an effect.

ALTER TABLE workload ADD COLUMN classification jsonb NOT NULL DEFAULT '[]'::jsonb;
