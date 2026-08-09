# AI Agent Coordinator

A Rust control-plane service for coordinating autonomous coding agents across many GitHub organizations and repositories.

The server combines:

1. An **OpenAI-compatible model gateway** that routes work among local models, Mistral, OpenRouter, OpenAI-compatible providers, and future adapters.
2. A **leased job queue** that lets ephemeral workers claim repository work, heartbeat while running, retry safely, and report results.
3. A **GitHub webhook intake** that turns labeled issues, labeled pull requests, and failed workflow runs into jobs.
4. **Cost and security policy** enforced per organization and repository before a request leaves the machine.
5. **Telemetry incident automation** that converts sustained Prometheus/Loki
   alerts into deduplicated GitHub and Linear tickets, then schedules
   feature-branch remediation through Gemini, Claude, and Codex.

The initial target is 20–30 GitHub organizations with approximately five repositories each. The coordinator is intentionally centralized while workers remain ephemeral.

## Architecture

```text
GitHub / Linear / operators
          |
          v
+-----------------------------+
| AI Agent Coordinator (Rust) |
|                             |
|  job queue + leases         |
|  budgets + usage ledger     |
|  secret scanning            |
|  route selection            |
+-------------+---------------+
              |
     +--------+---------+------------------+
     |                  |                  |
 local Ollama       Mistral/OpenRouter   frontier APIs
     |
     v
Ephemeral repository workers -> branch -> tests -> draft PR
```

The server does **not** store a broad GitHub personal access token. Workers should use a GitHub App installation token scoped to the organization/repositories they are assigned.

## Features

- Credential-free Agent Pontifex discovery at `GET /.well-known/agent-pontifex`
- OpenAI-compatible `POST /v1/chat/completions`
- Logical models: `auto`, `local`, `cheap`, `balanced`, `frontier`, or a configured model ID
- Task-specific model orders and explicit fallback chains
- Automatic downgrade when the preferred route is unavailable or over budget
- Per-organization and per-repository daily budgets
- Optional per-request budget with `x-fiducia-max-cost-usd`
- Secret detection and redaction before remote inference
- Restricted-data policy that can force local inference
- PostgreSQL-backed durable jobs and usage records through SeaORM
- Worker leases, heartbeats, retry delays, idempotent enqueueing, and transactional org/repo concurrency caps
- GitHub HMAC webhook verification
- Structured tracing and request IDs
- Docker and GitHub Actions examples

## Quick start

### 1. Configure

```bash
cp coordinator.example.yaml coordinator.yaml
cp .env.example .env
```

Set at least:

```bash
export COORDINATOR_API_TOKEN="$(openssl rand -hex 32)"
export GITHUB_WEBHOOK_SECRET="$(openssl rand -hex 32)"
export AI_AGENT_COORDINATOR_DATABASE_URL="postgres://postgres:postgres@127.0.0.1:5432/ai_agent_coordinator"
export MISTRAL_API_KEY="..."
```

Provider credentials are optional at startup. A provider whose required environment variable is missing is marked unavailable and skipped by the router. A local Ollama provider can run without a key.

Update every model price in `coordinator.yaml` to the provider’s current price before relying on budget enforcement.

### 2. Run

```bash
cargo run --release -- --config coordinator.yaml
```

Or:

```bash
# Keep the shared definitions checkout beside this repository; Compose mounts
# its canonical ai_agent_coordinator schema into the local PostgreSQL service.
docker compose up --build
```

### 3. Check health

```bash
curl http://localhost:8080/healthz
curl http://localhost:8080/readyz
curl http://localhost:8080/.well-known/agent-pontifex
```

## Agent Pontifex compatibility

The well-known endpoint advertises only the vendor-neutral leased-job contract:
create, claim, heartbeat, complete, cancel, retry, idempotency, and bounded leases.
It is intentionally public-safe and contains no credentials, tenant identifiers,
provider routes, budgets, GitHub administration, Linear delivery, Slack payloads,
or deployment topology.

The descriptor binds the canonical `coordinator` service to the
`agent-pontifex.coordinator` protocol and an explicit supported major-version
range. Agent Pontifex SDK clients fail closed when the service role, protocol, or
version range is incompatible. Product-specific behavior must remain in a
namespaced extension; this community descriptor currently advertises none.

After the shared protocol crate moves to `agent-pontifex/agent-sdk.rs`, this local
compatibility module should consume that crate rather than becoming an
independent protocol authority.

## Model gateway

The gateway accepts normal non-streaming OpenAI chat-completion bodies. Add repository context through headers:

```bash
curl http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer $COORDINATOR_API_TOKEN" \
  -H "Content-Type: application/json" \
  -H "x-fiducia-org: oresoftware" \
  -H "x-fiducia-repo: ai-agent-coordinator.rs" \
  -H "x-fiducia-task: code_change" \
  -H "x-fiducia-sensitivity: confidential" \
  -H "x-fiducia-max-cost-usd: 0.25" \
  -d '{
    "model": "auto",
    "messages": [
      {"role": "user", "content": "Review this patch and identify correctness risks."}
    ],
    "max_completion_tokens": 1200
  }'
```

Supported coordinator headers:

| Header | Meaning |
|---|---|
| `x-fiducia-org` | GitHub organization or tenant budget key |
| `x-fiducia-repo` | Repository name |
| `x-fiducia-task` | Routing class such as `classify`, `code_change`, `architecture`, or `security_review` |
| `x-fiducia-sensitivity` | `public`, `internal`, `confidential`, or `restricted` |
| `x-fiducia-allow-downgrade` | Whether fallback routes may be attempted; defaults to `true` |
| `x-fiducia-max-cost-usd` | Maximum estimated cost for this request |
| `x-request-id` | Optional caller-provided trace ID |

Responses include a `coordinator` object describing the selected route, estimated cost, secret scan, and attempted fallbacks.

Streaming is deliberately rejected in the first release so retries cannot accidentally produce duplicate partial output. Add streaming only after defining replay and billing semantics.

## Job queue

### Enqueue a job

```bash
curl http://localhost:8080/v1/jobs \
  -H "Authorization: Bearer $COORDINATOR_API_TOKEN" \
  -H "Content-Type: application/json" \
  -H "Idempotency-Key: linear:ENG-123" \
  -d '{
    "org": "oresoftware",
    "repo": "ai-agent-coordinator.rs",
    "task_type": "code_change",
    "priority": 25,
    "max_attempts": 3,
    "budget_usd": 2.00,
    "payload": {
      "ticket": "ENG-123",
      "goal": "Add Prometheus metrics"
    }
  }'
```

### Claim work

```bash
curl http://localhost:8080/v1/jobs/claim \
  -H "Authorization: Bearer $COORDINATOR_API_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "worker_id": "worker-us-east-1-07",
    "orgs": ["oresoftware"],
    "repositories": [],
    "lease_seconds": 180
  }'
```

A `204 No Content` response means there is currently no matching work.

### Heartbeat

```bash
curl http://localhost:8080/v1/jobs/JOB_ID/heartbeat \
  -H "Authorization: Bearer $COORDINATOR_API_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"worker_id":"worker-us-east-1-07","lease_seconds":180}'
```

### Complete or retry

```bash
curl http://localhost:8080/v1/jobs/JOB_ID/complete \
  -H "Authorization: Bearer $COORDINATOR_API_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "worker_id": "worker-us-east-1-07",
    "outcome": "failed",
    "error": "test environment unavailable",
    "retryable": true,
    "retry_delay_seconds": 60
  }'
```

Expired leases are returned to the queue until `max_attempts` is reached. Claims use serializable PostgreSQL transactions plus row locks to enforce the configured running-job cap for both the organization and repository across coordinator replicas.

## PostgreSQL schema and migrations

Runtime persistence uses SeaORM, but ORM entities are not migration authority.
The declarative PostgreSQL contract lives in the shared definitions repository:

```text
k8s-libs-and-shared-defs/
  pg-defs/schema/databases/ai_agent_coordinator/schema.sql
```

Every owned table is qualified by the `ai_agent_coordinator` PostgreSQL schema.
With the shared-defs checkout beside this repository, operators can review and
verify changes through [`scripts/dpm.sh`](scripts/dpm.sh):

```bash
export AI_AGENT_COORDINATOR_DATABASE_URL=postgres://...
export SHADOW_DATABASE_URL=postgres://.../postgres
scripts/dpm.sh diff
scripts/dpm.sh verify
```

Only `scripts/dpm.sh apply` changes the target, and dpm requires explicit
confirmation. The coordinator never runs DDL or migrations at startup.

## GitHub webhook setup

Configure a webhook for:

- Issues
- Pull requests
- Workflow runs

Point it at:

```text
https://coordinator.example.com/webhooks/github
```

Use the same secret stored in `GITHUB_WEBHOOK_SECRET`.

Default behavior:

- An issue carrying `agent:run` becomes a `github_issue` job.
- A pull request carrying `agent:review` becomes a `github_pr_review` job.
- A failed or timed-out workflow run becomes a high-priority `github_ci_failure` job.

GitHub delivery IDs become idempotency keys, so webhook retries do not duplicate jobs.

## Telemetry ticket automation

The optional Alertmanager intake creates redacted, fingerprinted incident jobs.
Gemini, Claude, and ChatGPT independently analyze each incident through the
multi-model bridge; a separate reviewer assignment synthesizes the ticket body.
GitHub and Linear records are cross-linked, and a 04:00 Eastern CronJob queues
ordered investigation, review, and implementation tasks. The implementation
stage is constrained to a feature branch, tests, GitHub Actions, and a draft
pull request.

See [`docs/telemetry-ticket-automation.md`](docs/telemetry-ticket-automation.md)
for the signal contract, protected configuration, routing rules, activation
sequence, and rollback controls.

## Recommended organization model

Use one organization coordinator policy per GitHub organization, not a permanently running process per repository.

- Maintain an org-level architecture graph for the roughly five related repositories.
- Start repo workers only when work exists.
- Restrict each worker’s GitHub App token to its assigned org/repository set.
- Cap global concurrency initially at 10–30 workers.
- Use separate queues or priority classes for incidents, CI failures, security work, and normal backlog work.
- Require independent review and human approval for authentication, payments, infrastructure, migrations, and secret-handling changes.

## Local model deployment

The example configuration expects an OpenAI-compatible Ollama endpoint:

```bash
ollama pull qwen2.5-coder:7b
ollama serve
```

For larger local models or GPU clusters, point the same provider entry at vLLM, llama.cpp server, LocalAI, or another OpenAI-compatible endpoint.

## Security notes

- Never place provider keys, GitHub tokens, repository secrets, or webhook secrets in YAML.
- Use environment variables or a secret manager.
- Prefer GitHub Apps over user PATs for workers.
- Keep the coordinator private behind a VPN, service mesh, or authenticated ingress.
- Set `restricted_requires_local: true` for source that cannot leave your environment.
- Secret scanning is defense in depth, not a replacement for least-privilege credentials.
- Rotate any credential pasted into chat, logs, tickets, or model prompts.

## Current limitations

- Chat completion streaming is not implemented.
- Online data backfills are not automated by the declarative schema workflow and must be planned separately.
- Token estimation before a request is approximate. Actual provider usage is recorded when the response supplies usage fields.
- Provider prices are configuration, not fetched automatically.
- Linear webhook ingestion and GitHub App token minting are planned adapters, not part of this bootstrap.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Cross-surface delivery

This repository is an HTTP control-plane service, not the native Rust desktop
app. User-visible or contract-changing job, lease, worker, budget, provider,
incident, approval, intervention, notification, permission, navigation, or
deep-link changes must be evaluated for:

- `agent-pontifex/agent-pontifex-flutter` on Android, iOS, Flutter Web/mobile
  web, and Flutter desktop when that proposed client is activated;
- `agent-pontifex/agent-pontifex-desktop.rs`, the proposed Rust desktop/operator
  client; and
- Agent Pontifex SDK/interfaces, generated clients, job/lease/worker/budget/
  incident/approval schemas, route types, synthetic incident fixtures, and
  conformance tests.

This is judgment-based coordination. Provider adapters, database/schema
internals, secret scanning internals, telemetry plumbing, and worker-only lease
mechanics may remain server-only. Native tray/background status, local worker
discovery, secure storage, logs, and keyboard workflows may be desktop-specific.
Job state, approvals, budget/quota status, intervention requests, incident state,
worker health, permissions, errors, notifications, and navigation normally
require coordinated changes or an explicit no-change rationale and parity
follow-up.

Mobile does not need every provider, repository, routing, or queue-management
control. High-risk policy and execution operations may remain authenticated
web/desktop-only while mobile receives status, notification, approval,
pause/cancel, and deep-link workflows. The proposed client repositories must not
be described as published until their remotes and builds are verified.

Deep links are HTTPS-first:

```text
https://<verified-agent-pontifex-owned-host>/open/<route>?<bounded-query>
```

The exact host must be verified before publication. A custom-scheme fallback
requires a reviewed ADR and must not be guessed. Control plane and future
clients must share versioned route types and fixtures and support cold start,
already-running delivery, authentication resume, replay/expiry rejection,
browser fallback, and explicit confirmation or reauthentication before job
execution, cancellation, retry, approval, policy, budget, provider, repository,
or incident actions.

Provider keys, GitHub tokens, webhook secrets, coordinator bearer tokens,
repository source, private prompts, raw incident logs, secret-scan findings,
worker credentials, database topology, and sensitive budget/usage details are
prohibited in URLs. Use bounded identifiers or short-lived, single-use,
audience-bound codes and validate route version, org/repo/job/worker/incident/
approval identity, action, authorization, assurance level, limits, and user
intent.

See [`docs/CROSS_SURFACE_DELIVERY.md`](docs/CROSS_SURFACE_DELIVERY.md) and the
[portfolio policy](https://github.com/ORESoftware/project-registry/blob/main/docs/cross-surface-delivery.md).

## License

MIT
