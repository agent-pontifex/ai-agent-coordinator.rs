# Cross-surface delivery

Verified **2026-08-06**.

## Surfaces

- Rust HTTP control plane: `agent-pontifex/ai-agent-coordinator.rs`
- Flutter Android/iOS, Flutter Web/mobile web, and Flutter desktop: `agent-pontifex/agent-pontifex-flutter` — proposed candidate
- Rust desktop/operator client: `agent-pontifex/agent-pontifex-desktop.rs` — proposed candidate
- Shared contracts: Agent Pontifex SDK/interfaces, generated clients, job/lease/worker/budget/incident/approval schemas, routes, synthetic incident fixtures, and conformance tests

This repository is a headless control-plane API, not the native Rust desktop implementation. Proposed client repository names must not be described as published until their remotes and builds are verified.

## Judgment-based propagation

Evaluate Flutter mobile, Flutter Web, Flutter desktop, Rust desktop, and shared contracts for every user-visible or contract-changing control-plane change. Provider adapters, database/schema internals, secret scanning internals, telemetry plumbing, and worker-only lease mechanics may remain server-only. Native tray/background status, local worker discovery, secure storage, logs, and operator keyboard workflows may be desktop-specific. Job state, approvals, budget/quota status, intervention requests, incident state, worker health, permissions, errors, notifications, and navigation normally propagate or require an explicit rationale and parity issue.

Mobile does not need every provider, repository, routing, or queue-management control. A good judgment call may keep high-risk policy and execution operations on the authenticated web/desktop surfaces while mobile receives status, notification, approval, pause/cancel, and deep-link workflows. Each issue and pull request records affected surfaces, omitted surfaces and rationale, accepted parity gaps, follow-up work, and separate platform/release status.

## Deep links

Canonical:

```text
https://<verified-agent-pontifex-owned-host>/open/<route>?<bounded-query>
```

The exact HTTPS host must be verified before publication. A custom-scheme fallback requires a reviewed ADR and must not be guessed. All surfaces share versioned route types and golden fixtures and support cold start, already-running delivery, authentication resume, replay/expiry rejection, browser fallback, and explicit confirmation or reauthentication before job execution, cancellation, retry, approval, policy, budget, provider, repository, or incident actions.

Never put provider keys, GitHub tokens, webhook secrets, coordinator bearer tokens, repository source, private prompts, raw incident logs, secret-scan findings, worker credentials, database topology, or sensitive budget/usage details in URLs. Use bounded identifiers or short-lived, single-use, audience-bound codes and validate route version, org/repo/job/worker/incident/approval IDs, action, authorization, assurance level, limits, and user intent.

## Review checklist

- [ ] Flutter Android/iOS impact evaluated.
- [ ] Flutter Web/mobile-web impact evaluated.
- [ ] Flutter desktop impact evaluated.
- [ ] Rust desktop/operator impact evaluated.
- [ ] Shared SDK/job/client/route/fixture impact evaluated.
- [ ] Deep-link, auth-resume, and operator-approval compatibility tested where relevant.
- [ ] High-risk operations omitted from mobile have a documented security/UX rationale.
- [ ] Omitted surfaces have a follow-up when needed.

## Routing

- GitHub Project: [`agent-pontifex-project` — Project 1](https://github.com/orgs/agent-pontifex/projects/1)
- Linear project: [`github.com/agent-pontifex`](https://linear.app/denman/project/githubcomagent-pontifex-1d2deb2be3c7)
- Central policy: [`cross-surface-delivery.md`](https://github.com/ORESoftware/project-registry/blob/main/docs/cross-surface-delivery.md)
- Desktop registry: [`desktop-applications.json`](https://github.com/ORESoftware/project-registry/blob/main/registry/desktop-applications.json)
