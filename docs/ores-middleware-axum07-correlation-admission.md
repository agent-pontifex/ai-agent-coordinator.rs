# Coordinator ORES middleware migration and correlation admission (#25)

The coordinator currently uses Axum **0.7** with tower-http **0.5**,
manually installed request IDs and TraceLayer, plus handler-level bearer
authorization. These semantics have NOT been replaced by the full ORES stack.

An upstream **Axum 0.7 adapter already exists** at
[ORESoftware/ores-middleware/src/rust/src/frameworks/axum07.rs](https://github.com/ORESoftware/ores-middleware/blob/main/src/rust/src/frameworks/axum07.rs).
It is feature-gated as `axum07`, and depends on `axum07` / `tower-http07`.
Therefore the integration decision is to **keep Axum 0.7 initially**, pin and
audit an exact reviewed upstream ORES middleware commit, use its adapter rather
than inventing a second lifecycle, and migrate only after compatibility tests.
A new Cargo dependency, runtime config, or global exporter is deliberately
**not** added before an exact-source dependency/lockfile and behavior review.

## Implemented in this PR

The coordinator now admits inbound telemetry headers through
`src/correlation.rs` **before** TraceLayer and
SetRequestId/PropagateRequestId:
- Keep only canonical lowercase UUIDv4 caller request IDs, otherwise let the
  existing UUID generator create a replacement. Reject unbounded arbitrary
  strings, private source, prompt or token material as an ID.
- Keep only structurally valid W3C version-00 `traceparent` with nonzero
  trace and span IDs. Drop invalid or unsupported versions, including binary
  and Unicode payloads.
- Drop all untrusted `baggage` and `tracestate` before telemetry; the future
  reviewed trusted-proxy admission policy may selectively re-enable bounded
  `otel.*` fields. No arbitrary headers are logged or forwarded.
- Public `/healthz` and discovery routes remain public. This middleware has
  no authentication/authorization side effects.

This is a temporary **ingress safety gate**, not a fork of ORES middleware's
execution model and not a claim that W3C context is already saved in durable
jobs.

## Required next phases (not yet implemented)

1. Pin the reviewed ORES middleware and ORES OTel revisions in Cargo.lock,
   integrate the existing upstream `axum07` adapter, and prove consumer-owned
   stage order and public/protected route behavior with real HTTP tests.
2. Propagate validated request/trace IDs through durable create/claim/heartbeat/
   complete; include explicit schema migration and record idempotency proofs.
3. Ensure application-owned exporters emit only bounded org/repo/job/worker
   IDs, status/durations/counters. Add negative tests proving secret tokens,
   prompts and repository-source canaries never reach exporter payloads.
4. Admit trusted transport metadata only from an authenticated edge; separate
   client-supplied IDs from operator-authenticated correlation, and do not
   use baggage or trace IDs as authorization authorities.
5. Benchmark and verify middleware ordering, cancellation, deadline/body
   limits, recovery, quotas, and redaction under exact-head CI before making
   any middleware rollout the default.

Run the new tests: `cargo test correlation::tests`. Full
`cargo fmt --all -- --check`, Clippy and integration tests remain required.
