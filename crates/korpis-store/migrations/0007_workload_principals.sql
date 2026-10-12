-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The workload tables catch up with 0005: who declared an intent, set a power declaration, or
-- computed and approved a plan is stored as a principal key, as §3.1 of docs/03-state.md already
-- shows for `declared_by`, not as the JSON of a wire message.
--
-- A plan also records what it needs before it applies (§3.3 of docs/01-model.md): `auto`,
-- `confirmation`, or `authority` within `approval_scope`, and why in `approval_reason`. Approving
-- moves the target by compare-and-swap on `target_version` (§3.2 of docs/03-state.md), so the
-- workload keeps it beside the pointer, as it already did.

ALTER TABLE intent ALTER COLUMN declared_by TYPE text USING declared_by #>> '{}';
ALTER TABLE power ALTER COLUMN set_by TYPE text USING set_by #>> '{}';
ALTER TABLE plan ALTER COLUMN computed_by TYPE text USING computed_by #>> '{}';
ALTER TABLE plan ALTER COLUMN approved_by TYPE text USING approved_by #>> '{}';

ALTER TABLE plan
    ADD COLUMN approval text NOT NULL DEFAULT 'confirmation'
        CHECK (approval IN ('auto', 'confirmation', 'authority')),
    ADD COLUMN approval_scope jsonb,
    ADD COLUMN approval_reason text NOT NULL DEFAULT '',
    ADD CONSTRAINT plan_authority_has_scope
        CHECK ((approval = 'authority') = (approval_scope IS NOT NULL));
ALTER TABLE plan ALTER COLUMN approval DROP DEFAULT;
ALTER TABLE plan ALTER COLUMN approval_reason DROP DEFAULT;
