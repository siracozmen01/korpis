-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0101: the sessions people held end, since only an account may hold one again.

DELETE FROM user_session WHERE binding_id IS NOT NULL;
DROP INDEX user_session_binding;
ALTER TABLE user_session
    DROP CONSTRAINT user_session_sets,
    DROP CONSTRAINT user_session_held,
    DROP COLUMN sets,
    DROP COLUMN binding_id;
ALTER TABLE user_session ALTER COLUMN account_id SET NOT NULL;
