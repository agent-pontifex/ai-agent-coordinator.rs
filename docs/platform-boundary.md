# Agent Pontifex platform boundary

This document explains the machine-readable contract in
[`platform-boundary.json`](platform-boundary.json). It is tracked by Linear issue
`DEN-1873` and starts as `proposed-defaults` so the SDK, bridge, coordinator, Fiducia,
CI-continuity, and cluster owners can review it without silently changing a runtime API.

## Role

`ai-agent-coordinator` is the **agent-orchestration control plane**. It accepts coding-agent
work, applies organization and repository policy, selects an allowed model route, maintains
job history and worker claims, validates and persists task/evidence envelopes, and records
approvals and evidence.

It is not the canonical public Agent Pontifex protocol authority, a GitHub Actions
implementation, a general shell executor, a Kubernetes control plane, a human identity
provider, a product business API, or the owner of Fiducia's Raft state.

```text
agent-pontifex/agent-sdk.rs
    canonical discovery + leased-job protocol
                     |
                     v
GitHub / Linear / operator
           |
           v
+------------------------------+
| Agent Pontifex coordinator   |
|                              |
| policy + budgets             |
| model routing                |
| task history + approvals     |
| protocol validation          |
| worker claims + evidence     |
+------+---------------+-------+
       |               |
       |               +----> model provider after policy/redaction
       |
       +----> ephemeral coding worker
       |
       +----> gha-indie-worker for reviewed workflow execution
       |
       +----> Fiducia for cross-process fencing at irreversible effects
       |
       +----> k8s-cluster for deployment tenancy and shared backends
```

## Protocol boundary

`agent-pontifex/agent-sdk.rs` is the canonical public protocol authority. The coordinator
consumes that protocol from an immutable source revision or released package; it does not
redefine public discovery, service-role, supported-version, or leased-job types locally.

Discovery and protocol negotiation fail closed when the advertised service role, protocol,
or supported major-version range is incompatible. Public leased-job fields come from the
canonical SDK. Coordinator-only policy—such as tenant capability grants, sensitivity,
approval, immutable target, fencing, and side-effect evidence—lives in a namespaced private
extension around the public envelope rather than forking it.

The current local compatibility module may validate a pinned SDK wire shape during migration,
but it is not an independent protocol authority. Once the shared protocol crate is consumed
directly, duplicate local type definitions should be removed rather than maintained as a
second source of truth.

## GitHub boundary

GitHub access uses short-lived installation tokens scoped to the assigned organization and
minimum repository set. A task or model prompt never contains a GitHub App private key,
personal access token, or ambient host credential.

The coordinator may request repository metadata, inspect canonical issue/PR context, create a
feature branch, push bounded commits, open a pull request, and read checks when the capability
grant allows the exact operation. It does not infer permission merely from repository
visibility.

Every mutation is tied to an actor, tenant, repository, capability grant, immutable target
when applicable, idempotency key, approval when required, trace, and terminal evidence.

## CI boundary

The coordinator decides **that** a reviewed workflow or fixed profile should execute.
`gha-indie-worker` owns **how** GitHub Actions-compatible work executes.

A dispatch contains only:

- repository;
- full immutable commit SHA;
- reviewed workflow path or fixed profile;
- idempotency key;
- trace context.

It does not contain caller-selected shell, mutable branch authority, arbitrary marketplace
action code for the independent lane, a caller-selected runner image, or a Kubernetes
manifest.

The normal parity lane remains Actions Runner Controller. The independent lane remains a
bounded, fail-closed compiler to reviewed build profiles. Unsupported semantics are reported;
they are never approximated silently.

## Fiducia boundary

PostgreSQL row locks and serializable transactions are sufficient for a local durable job
claim whose transaction contains the complete side effect. They are not sufficient proof
that two coordinator replicas cannot both authorize an external merge, deployment, release,
or credential change after a lease race.

Fiducia fencing is required when:

- multiple replicas can perform the same irreversible external mutation;
- webhook-delivery claims are shared across replicas;
- an executor must prove it still holds authority at the final side-effect boundary.

Read-only planning, dry-run validation, and a single PostgreSQL transaction with no external
side effect do not require a Fiducia round trip.

## Identity and authorization

`shared-auth` is the portfolio human identity authority. The coordinator owns its own service
and capability authorization. Product APIs retain product authorization. A human identity or
role claim is context for a policy decision, not an unrestricted coding-agent grant.

A capability grant binds at least tenant, repository, actor, task type, allowed tools,
resources, environment, issue or request context, expiry, and approval requirements. Unknown,
expired, stale, or mismatched grants fail before dispatch.

## Side-effect sequence

```text
validate request and expiry
        |
resolve actor + capability + canonical issue
        |
validate canonical Agent Pontifex protocol envelope
        |
classify sensitivity and scan secrets
        |
plan bounded operation against immutable revision
        |
obtain approval when required
        |
obtain current fencing token when execution can race
        |
dispatch through the owning adapter
        |
reconcile ambiguous outcomes; never retry blindly
        |
write terminal evidence and audit receipt
```

Cancellation stops new dispatch, revokes pending approvals, and reconciles work already in
flight. A lost worker lease does not make an old worker safe to retry; stale authority must be
made unable to complete the protected side effect first.

## Observability and audit

Operational telemetry propagates W3C trace context and records bounded route, timing, cost,
and outcome information. It excludes credentials, authorization headers, private keys,
repository secrets, raw secret-bearing prompts or outputs, and hidden chain-of-thought.

Actor, tenant, repository, request, job, and commit identifiers are not Prometheus or Loki
stream labels. They may appear as access-controlled event fields or span attributes when
necessary for investigation.

Branches, pushes, pull requests, merges, deployments, releases, credentials, and policy
changes produce append-only audit receipts. Operational logs are not the audit authority.

## Deployment

The coordinator image is built once, tested and signed as that exact artifact, then promoted
by immutable digest. The runtime uses a dedicated service account, namespace-scoped RBAC,
network egress policy, ExternalSecret-managed credentials, and separate coordinator and
worker identities.

Before horizontally scaling external mutations, in-memory deduplication must be replaced by a
shared durable claim or a Fiducia-fenced claim. Scaling HTTP replicas without scaling the
side-effect authority is not safe.
