# Repository agent instructions

These instructions apply to `ORESoftware/ai-agent-coordinator.rs` and every path beneath it unless a more specific descendant `agents.md` adds narrower rules.

## Discover instructions hierarchically

Resolve `$PWD`, then walk upward through every parent directory to the filesystem root. Read every readable lowercase `agents.md` on that ancestor chain and apply them in root-to-leaf order. Do not search siblings. Deduplicate resolved paths and inodes, detect symlink cycles, and report unreadable instruction files instead of silently skipping them.

## Synchronize and merge safely

Before editing, inspect the current branch, working tree, remotes, default branch, relevant Linear issue, and open pull requests. Fetch remote state and create a focused feature branch from the latest reviewed `main`.

- avoid git rebase in favor of git merge.
- Never force-push, rewrite shared history, discard concurrent work, bypass review, or bypass required checks unless the user explicitly authorizes that exact action.
- Resolve conflicts semantically by combining the compatible intent, invariants, tests, documentation, configuration, and API contracts from both sides. Never resolve a conflict merely by choosing `ours`, `theirs`, current, or incoming.
- After a merge or conflict resolution, reread every affected file from the top and scan the entire worktree for unresolved markers, excluding `.git`:

```sh
grep -RInE '^(<<<<<<<|=======|>>>>>>>)' --exclude-dir=.git .
```

A merge is complete only when the resulting system is conceptually coherent and all relevant validation passes.

## Preserve the coordinator architecture

This repository is a Rust control plane built around Axum, Tokio, SeaORM/PostgreSQL, authenticated GitHub and Linear integrations, leased work queues, provider routing, budget enforcement, and bounded automation.

- Keep HTTP handlers thin; place policy, persistence, connector, and orchestration behavior in explicit modules with testable boundaries.
- Preserve request IDs, structured errors, bounded payloads, timeouts, retries, idempotency keys, and deterministic recovery for all external calls.
- Treat queue claims, heartbeats, completion, retries, and concurrency caps as one transactional lease protocol. Do not introduce a mutation path that can ignore ownership, lease expiry, or idempotency.
- SeaORM entities are runtime adapters, not schema authority. The canonical `ai_agent_coordinator` PostgreSQL schema lives in `ORESoftware/k8s-libs-and-shared-defs`; the application must not create or migrate tables at startup.
- Keep deployment, image, database, schema, secret, and state-migration changes coherent. Do not remove or replace a storage contract until the promoted image, protected configuration, migration evidence, readiness checks, and rollback path agree.

## Enforce security and cost boundaries

- Never commit, print, echo, serialize, or place credentials in command-line arguments, pull-request text, Linear, fixtures, workflow inputs, logs, or telemetry.
- Use environment or approved secret-manager injection for tokens and database URLs. Redact sensitive values before errors or diagnostics can include them.
- Do not store or distribute a broad GitHub personal access token. Use least-privilege GitHub App installation credentials scoped to the authorized organization and repositories.
- Keep repository administration, Linear mutation delivery, telemetry remediation, and other externally visible writes disabled or dry-run by default. Require exact allowlists, explicit confirmation, durable idempotency, and auditable evidence before enabling live behavior.
- Preserve data-sensitivity routing, secret scanning, provider availability checks, per-organization and per-repository budgets, request cost ceilings, and fail-closed behavior. Restricted content must not be sent to an unapproved remote provider.
- Bound telemetry labels and stored prompt or incident metadata. Do not persist hidden reasoning, unnecessary personal data, customer payloads, or secrets.

## Protect GitHub, Linear, and agent workflows

- Connector evidence must identify resolvable repositories, branches, pull requests, commits, checks, and Linear entities. Local archives, local-only branches, chat claims, or unverifiable hashes are not delivery evidence.
- Search for existing Linear and GitHub work before creating new records. Amend the canonical item, transfer unique scope before marking duplicates, and use real relations rather than title similarity.
- Keep webhook verification constant-time and organization-scoped. Enforce delivery IDs or equivalent durable mutation keys so retries cannot duplicate work.
- Automated remediation may create bounded feature branches and draft pull requests only. It must not merge, deploy, send outbound messages, apply for jobs, create repositories, or change external state beyond the reviewed policy gate.
- Preserve exact repository/default-branch allowlists and the distinction between planning, dry-run, and apply modes.

## Test and document behavior

Keep changes focused and update tests, documentation, sample configuration, deployment contracts, and observability whenever behavior changes.

Run the smallest relevant checks while iterating, then the complete applicable gate before requesting merge:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all
python3 -m py_compile scripts/*.py
python3 -m unittest -v scripts/test_validate_agents_md.py
python3 scripts/validate_agents_md.py --repo-root . --start-dir src
```

Validate every changed GitHub Actions workflow with the repository's pinned `actionlint` contract. Record exact-head check evidence, semantic conflict decisions, residual risk, and intentionally deferred operational work in the pull request and matching Linear issue.
