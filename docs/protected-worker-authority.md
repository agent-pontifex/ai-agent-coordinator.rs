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
the secret lookup resolves each bearer, validates 32–512 visible ASCII bytes,
computes a domain-separated SHA-256 digest, and drops the raw value. The
registry stores no replayable bearer and its `Debug` implementation redacts the
digests.

The coordinator-wide administrative bearer cannot alias any protected-worker
credential. It remains usable for existing unprotected work, but it cannot
claim, heartbeat, or complete a protected task. An absent registry similarly
preserves unprotected work while making every protected task ineligible.

## Request behavior

Protected claims require an explicit task filter containing only the exact task
bound to the authenticated worker. Empty, mixed protected/unprotected, foreign,
or impersonated filters fail closed. Heartbeat and completion authorization
rechecks the current job task and exact lease holder before persistence applies
its own compare-and-set conditions.

## Activation sequence

This contract is non-activating. Production enablement still requires all of
the following in separate reviewed changes:

1. Add the registry to configuration startup and fail startup when a declared
   role or credential is incomplete.
2. Route claim, heartbeat, and completion through the registry and carry the
   normalized server-side worker identity into persistence.
3. Extend persistence tests so broad administrative claims cannot select a
   protected task and stale or foreign workers cannot renew or complete it.
4. Provision six disjoint service identities and credentials through the
   approved encrypted-secret lifecycle; never commit bearer or private-key
   material.
5. Pin canonical public Ed25519 SPKI fingerprints and verify signed artifacts
   in finalizers before any Linear or GitHub side effect.
6. Prove durable replay rejection, exact source/image identity, disposable
   canaries, rollback, and a separately reviewed deployment activation.

Until those gates land, no protected worker is authorized or deployed by this
module.
