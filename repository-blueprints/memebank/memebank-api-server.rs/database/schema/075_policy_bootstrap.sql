BEGIN;

CREATE POLICY memberships_app_owner_bootstrap ON memebank.library_memberships
    FOR INSERT TO mb_app
    WITH CHECK (
        role = 'owner'
        AND status = 'active'
        AND user_id = memebank_private.current_user_id()
        AND EXISTS (
            SELECT 1
            FROM memebank.libraries AS library
            WHERE library.id = library_id
              AND library.owner_user_id = memebank_private.current_user_id()
        )
    );

GRANT EXECUTE ON FUNCTION memebank_private.current_user_id() TO mb_policy_owner;
GRANT EXECUTE ON FUNCTION memebank_private.current_worker_library_id() TO mb_policy_owner;
GRANT EXECUTE ON FUNCTION memebank_private.worker_has_library_access(uuid) TO mb_policy_owner;

COMMIT;
