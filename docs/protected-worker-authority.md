# Protected worker authority

The coordinator's protected-worker registry is the server-side authority for
opinion, readiness, and reconciliation roles. Job payloads, request bodies, and
caller-provided `worker_id` values are not identity sources.

## Protected task and role matrix

| Task type | Role | Provider authority | Signing authority | Mutation authority |
| --- | --- | --- | --- | --- |
| `linear_opinion_chatgpt` | ChatGPT opinion worker | OpenAI only | one unique key/fingerprint | none |
| `linear_opinion_claude` | Claude opinion worker | Anthropic only | one unique key/fingerprint | none |
| `pr_readiness_primary` | readiness primary | one configured provider | one unique key/fingerprint | none |
| `pr_readiness_critic` | readiness critic | one configured provider | one unique key/fingerprint | none |
| `reconciliation_linear_finalizer` | Linear finalizer | none | none | Linear only |
| `reconciliation_github_finalizer` | GitHub finalizer | none | none | GitHub only |

A non-empty registry must configure all six roles exactly once. Worker IDs,
credential environment names and digests, trust domains, signing key IDs, and
canonical public-key fingerprints must be distinct. Finalizers cannot hold
provider or opinion-signing capabilities, and signers cannot hold Linear or
GitHub mutation capabilities.

## Credential boundary

The public configuration contains only environment-variable names. At startup,
the secret lookup resolves each worker bearer, validates 32–512 visible ASCII
bytes, computes a domain-separated SHA-256 digest, and drops the raw value. The
registry stores no replayable bearer and its `Debug` implementation redacts the
digests.

The coordinator-wide administrative bearer cannot alias any protected-worker
credential. It remains usable for existing unprotected work, but it cannot
claim, heartbeat, or complete a protected task. An absent registry similarly
preserves unprotected work while making every protected task ineligible.

## Request behavior

Protected claims require an explicit task filter containing only the exact task
bound to the authenticated worker. Empty, mixed protected/unprotected, foreign,
or impersonated filters fail closed. A presented credential is authenticated
before the job is loaded, so unauthenticated callers cannot use mutation routes
as a job-existence oracle. Heartbeat and completion authorization rechecks the
current job task, exact lease holder, positive `lease_attempt`, and unexpired
lease before persistence applies the same worker, attempt, status, and expiry
conditions. The claim response's `job.attempts` value is the lease fence that a
protected worker must echo.

Generic database claims use an explicit `ExcludeProtected` policy even when the
request has no task filter. A protected task is selectable only through
`claim_job_authorized` with the exact server-derived role policy. Authorization,
organization, repository, and task filters are applied before the bounded
`FOR UPDATE SKIP LOCKED` candidate window. PostgreSQL regressions place more
than one full candidate window of higher-priority protected jobs ahead of an
ordinary job and prove that a broad worker still receives the ordinary job.
They also prove that expired leases and stale claim generations cannot heartbeat
or complete a reacquired lease.

## Hardening review disposition

The exact-head hardening review closed four fail-closed gaps without activating
any worker or external authority: legacy administrative credentials retain their
bounded compatibility range while protected-worker bearers retain the stronger
minimum; authentication precedes resource lookup; mutation compare-and-set
includes the current positive lease generation and an unexpired lease; and all
authority filters execute before the bounded lock window. Dedicated unit and
PostgreSQL regressions cover each condition, including a protected backlog larger
than the candidate window.

## Runtime enforcement status

The stacked runtime change completes the source-level enforcement steps:

1. Configuration startup constructs the closed registry from public profile
   metadata and declared environment-variable names. An incomplete non-empty
   registry fails startup.
2. Claim requests are normalized through the authenticated profile before
   database selection. Heartbeat and completion load the current job, verify
   the exact task and lease holder, and pass the server-derived worker identity
   to persistence.
3. Default database claims exclude protected tasks, and focused unit plus
   PostgreSQL tests cover broad-claim rejection, impersonation, mixed filters,
   foreign roles, stale ownership, and credential aliasing.

These are authorization mechanics, not production activation. No bearer value,
private signing key, provider call, finalizer mutation credential, worker
process, or deployment is introduced by the source change.

## Remaining activation sequence

Production enablement still requires all of the following in separately
reviewed changes:

1. Provision six disjoint service identities and credentials through the
   approved encrypted-secret lifecycle; never commit bearer or private-key
   material.
2. Pin canonical public Ed25519 SPKI fingerprints and verify signed artifacts
   in finalizers before any Linear or GitHub side effect.
3. Persist and reject signed-artifact replay across process and database
   restarts.
4. Pin exact source and image identities for every signer and finalizer.
5. Run disposable provider, verification, Linear, and GitHub canaries with
   bounded authority and reviewed rollback.
6. Activate deployment only through a separately reviewed GitOps change.

Until those gates land, no protected worker is authorized or deployed by this
module.
