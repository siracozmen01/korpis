-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The inverse of 0075: the owner's own grant of korpis_app is revoked, which leaves what creating
-- the role gave it (on PostgreSQL 16 and later, the right to administer it), as before 0075.

DO $$
BEGIN
    IF (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) THEN
        RETURN;
    END IF;
    EXECUTE format('REVOKE korpis_app FROM %I GRANTED BY %I', current_user, current_user);
END
$$;
