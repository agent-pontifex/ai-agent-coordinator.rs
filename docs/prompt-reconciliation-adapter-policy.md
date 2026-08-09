# Authenticated reconciliation adapter policy

The authenticated adapter layer must remain narrower than the planner. It receives an already-reviewed operation and may only collect allowlisted evidence or apply the one authorized Linear mutation.

## GitHub read policy

- credentials are loaded from environment only and render as `[REDACTED]` under `Debug`;
- repositories must match a normalized exact `owner/repository` allowlist;
- the endpoint must be the pinned HTTPS host;
- HTTP is accepted only for explicit loopback test servers;
- redirects are disabled and any redirect target is refused rather than followed;
- response bytes and page counts are bounded before parsing;
- only safe reads may retry, and only for explicit transient transport/status failures;
- `Retry-After` is bounded;
- evidence snapshots use deterministic ordering and feed the DEN-1609 planner without mutation.

## Linear write policy

- account fingerprint, exact plan digest, and exact confirmation phrase are rechecked immediately before mutation;
- canonical issue lookup and update happen before create;
- a final duplicate search happens immediately before create;
- mutations carry a stable nonsecret operation marker;
- transport failure after a write begins is ambiguous and is never retried automatically;
- the canonical remote result must be known before an idempotency receipt is recorded;
- a rerun with a matching receipt is a no-op; a conflicting result fails closed.

## Failure-injection matrix

The adapter tests must cover 401/403, 404, 408, 409, 425, 429 with bounded `Retry-After`, transient 5xx, redirect, oversized body, pagination exhaustion, timeout before a request, ambiguous timeout after a mutation begins, malformed JSON, duplicate-race discovery, allowlist refusal, and receipt replay.