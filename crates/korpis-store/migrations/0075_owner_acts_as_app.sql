-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The schema's owner acts as korpis_app (§3.1 of docs/03-state.md) when it installs, as every
-- store connection does. A superuser may set any role; an owner that is not one, with CREATEROLE,
-- as installation provisions it (§2 of docs/18-operations.md), may set korpis_app only as a
-- member. On PostgreSQL 16 and later, creating korpis_app gave the owner the right to administer
-- it and not to set it (createrole_self_grant is empty by default), so it is granted here with
-- SET and without INHERIT: the owner acts as the application role only when it asks to. Before
-- 16, membership is all SET ROLE needs. Finding 93 of docs/23-walkthroughs.md.

DO $$
BEGIN
    IF (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) THEN
        RETURN;
    END IF;
    IF current_setting('server_version_num')::int >= 160000 THEN
        EXECUTE format('GRANT korpis_app TO %I WITH SET TRUE, INHERIT FALSE', current_user);
    ELSIF NOT pg_has_role(current_user, 'korpis_app', 'MEMBER') THEN
        EXECUTE format('GRANT korpis_app TO %I', current_user);
    END IF;
END
$$;
