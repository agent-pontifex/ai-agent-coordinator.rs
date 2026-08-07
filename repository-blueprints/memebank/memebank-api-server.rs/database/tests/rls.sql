\set ON_ERROR_STOP on

BEGIN;
SET LOCAL ROLE mb_app;
SET LOCAL memebank.user_id = '20000000-0000-4000-8000-000000000001';

SELECT count(*) = 1 AS app_sees_only_alpha
FROM memebank.assets
\gset
\if :app_sees_only_alpha
\else
    \echo 'mb_app tenant isolation failed: Alpha user saw an unexpected asset count'
    \quit 3
\endif

WITH attempted AS (
    UPDATE memebank.assets
    SET note = 'cross-tenant write must not occur'
    WHERE id = '40000000-0000-4000-8000-000000000002'
    RETURNING 1
)
SELECT count(*) = 0 AS cross_tenant_update_blocked FROM attempted
\gset
\if :cross_tenant_update_blocked
\else
    \echo 'mb_app cross-tenant update was not blocked'
    \quit 3
\endif

INSERT INTO memebank.libraries (
    id, owner_user_id, name, visibility
)
VALUES (
    '10000000-0000-4000-8000-000000000003',
    '20000000-0000-4000-8000-000000000001',
    'Owner bootstrap test',
    'private'
);

INSERT INTO memebank.library_memberships (
    library_id, user_id, role, status, joined_at
)
VALUES (
    '10000000-0000-4000-8000-000000000003',
    '20000000-0000-4000-8000-000000000001',
    'owner',
    'active',
    clock_timestamp()
);

SELECT memebank_private.has_library_access(
    '10000000-0000-4000-8000-000000000003',
    true,
    true
) AS owner_bootstrap_succeeded
\gset
\if :owner_bootstrap_succeeded
\else
    \echo 'new library owner membership bootstrap failed'
    \quit 3
\endif
ROLLBACK;

BEGIN;
SET LOCAL ROLE mb_worker;
SET LOCAL memebank.library_id = '10000000-0000-4000-8000-000000000001';

SELECT count(*) = 1 AS worker_sees_scoped_library
FROM memebank.assets
\gset
\if :worker_sees_scoped_library
\else
    \echo 'mb_worker library scope did not isolate assets'
    \quit 3
\endif

SELECT count(*) = 1 AS claimed_one_scoped_job
FROM memebank.claim_jobs('rls-fixture-worker', 1, 60)
\gset
\if :claimed_one_scoped_job
\else
    \echo 'scoped worker did not claim exactly one job'
    \quit 3
\endif

SELECT count(*) = 0 AS worker_cannot_see_beta
FROM memebank.assets
WHERE library_id = '10000000-0000-4000-8000-000000000002'
\gset
\if :worker_cannot_see_beta
\else
    \echo 'mb_worker confused-deputy isolation failed'
    \quit 3
\endif
ROLLBACK;

BEGIN;
SET LOCAL ROLE mb_worker;
DO $missing_scope$
BEGIN
    BEGIN
        PERFORM * FROM memebank.claim_jobs('unscoped-worker', 1, 60);
        RAISE EXCEPTION 'claim_jobs unexpectedly accepted an unscoped worker';
    EXCEPTION
        WHEN insufficient_privilege THEN
            NULL;
    END;
END
$missing_scope$;
ROLLBACK;
