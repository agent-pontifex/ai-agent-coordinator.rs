# Promote the MemeBank PostgreSQL desired state

**Tracking:** DEN-1007, DEN-1006, DEN-1010, DEN-1012, DEN-1005, and DEN-1043

The staged database contract under `repository-blueprints/memebank/memebank-api-server.rs/database` is reviewed source, not a deployed database or canonical API repository.

## Preconditions

1. `github.com/memebank/memebank-api-server.rs` exists with reviewed visibility and `main` initialization evidence.
2. The connected GitHub App can read/write contents, branches, pull requests, issues, and Actions in the target repository.
3. The exact `mb-interfaces` contract version consumed by the API is pinned.
4. PostgreSQL 16+ and an approved pgvector version are pinned in local development and CI.
5. Database roles are provisioned through protected infrastructure; no credentials appear in source, workflow arguments, logs, or Linear.

## Promotion

Import the exact reviewed database tree through a traceable subtree/history-preserving method. Record the coordinator source commit in the target PR. Do not move the work through an untracked archive.

The target PR must run the static contract gate plus a real empty-database job that:

1. starts the pinned PostgreSQL/pgvector image;
2. applies `bootstrap/roles.sql` under an administrative test connection;
3. renders and applies the complete ordered desired state once;
4. executes `tests/verify_schema.sql`;
5. loads `seeds/representative.sql`;
6. executes `tests/rls.sql` under the intended roles;
7. generates SeaORM entities and fails on drift;
8. captures representative hybrid-search and job-claim query plans;
9. tears down without retaining credentials or fixture data.

## Production transition evidence

Before production apply, generate a reviewed desired-state diff from the live schema. Classify every operation as additive, backfill, cutover, validation, or destructive. Destructive operations require explicit approval, backup/restore evidence, and a rollback path.

Required Linear evidence:

- canonical repository URL and repository ID;
- source blueprint commit and rendered schema SHA-256;
- target PR and exact merged head;
- PostgreSQL, pgvector, SeaORM, and tool versions;
- empty-database CI and RLS test runs;
- representative query-plan artifacts;
- backup/restore result;
- model-index backfill/cutover/rollback evidence;
- explicit statement that production services do not auto-migrate on startup.

DEN-1007 remains open until this evidence is attached from the canonical repository and a real database environment.
