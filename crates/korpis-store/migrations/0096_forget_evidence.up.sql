-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- An identity provider's call carries what a person presented (§5.2 of docs/08-identity.md): a
-- signed chat interaction, an authorization code. It is needed only while the call is queued and
-- sent; once the call finishes, however it finishes, its evidence and headers are emptied, so a
-- finished call kept for its readers holds no credential. A trigger rather than each statement
-- that finishes a call, so no way of finishing one can forget to.

CREATE FUNCTION korpis_forget_evidence() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.finished_at IS NOT NULL AND NEW.request ? 'assert' THEN
        NEW.request := jsonb_set(
            jsonb_set(NEW.request, '{assert,evidence}', '""'), '{assert,headers}', '{}');
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER provider_call_forget_evidence BEFORE INSERT OR UPDATE ON provider_call
    FOR EACH ROW EXECUTE FUNCTION korpis_forget_evidence();
